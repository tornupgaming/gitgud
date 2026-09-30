//! Central diff viewer (the DIFF ANALYSIS module). The diff arrives as nested
//! `DiffData`; we flatten it once into `DiffView` rows -- computing syntax
//! highlight spans in the same pass -- so the per-frame render is a plain
//! index walk that `show_rows` can virtualize.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    Align, Align2, CornerRadius, Layout, Rect, Sense, Spinner, Stroke, StrokeKind, TextStyle, Ui,
    pos2, vec2,
};

use super::highlight::{LineHighlighter, Spans};
use super::{HudButton, char_w, hvscroll, module, text_width, truncate_right};
use crate::app::RepoTab;
use crate::git::{DiffData, DiffKey, DiffLine, LineKind, RepoId};
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
    /// Widest row in characters; sizes the horizontal scroll extent.
    pub max_chars: usize,
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
        let mut max_chars = 0;
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
            }
            // A binary file can carry the collector's truncation marker.
            rows.extend(f.lines.into_iter().map(|l| {
                max_chars = max_chars.max(l.text.chars().count());
                let spans = (l.kind != LineKind::Hunk)
                    .then(|| hl.spans(&l.text))
                    .flatten();
                DiffRow::Line(l, spans)
            }));
        }
        Self {
            adds: data.adds,
            dels: data.dels,
            rows,
            max_chars,
        }
    }
}

/// A commit file's metadata and shared render rows. Raw line strings are moved
/// into the view, so selecting a file never copies or re-highlights its content.
pub struct PreparedFile {
    pub path: String,
    pub adds: usize,
    pub dels: usize,
    pub view: Arc<DiffView>,
}

pub struct PreparedDiff {
    pub files: Vec<PreparedFile>,
    pub adds: usize,
    pub dels: usize,
}

impl PreparedDiff {
    fn build(data: DiffData) -> Self {
        let files = data
            .files
            .into_iter()
            .map(|file| {
                let path = file.path.clone();
                let adds = file.adds;
                let dels = file.dels;
                let view = Arc::new(DiffView::build(DiffData {
                    files: vec![file],
                    adds,
                    dels,
                }));
                PreparedFile {
                    path,
                    adds,
                    dels,
                    view,
                }
            })
            .collect();
        Self {
            files,
            adds: data.adds,
            dels: data.dels,
        }
    }

    pub fn file_view(&self, path: &str) -> Option<Arc<DiffView>> {
        self.files
            .iter()
            .find(|f| f.path == path)
            .map(|f| Arc::clone(&f.view))
    }
}

pub enum PreparedSelection {
    Workdir(Arc<DiffView>),
    Commit(PreparedDiff),
}

pub struct PreparedReply {
    pub id: RepoId,
    pub key: DiffKey,
    pub view: PreparedSelection,
}

/// One preparation thread for the app, independent of the repository worker.
/// Syntax loading, parsing and flattening never run during frame layout.
pub struct DiffWorker {
    jobs: Sender<(RepoId, DiffKey, DiffData)>,
    replies: Receiver<PreparedReply>,
}

impl DiffWorker {
    pub fn spawn(ctx: eframe::egui::Context) -> Self {
        let (jobs, rx) = channel::<(RepoId, DiffKey, DiffData)>();
        let (tx, replies) = channel();
        std::thread::spawn(move || {
            while let Ok((id, key, data)) = rx.recv() {
                let view = if matches!(key, DiffKey::Commit { .. }) {
                    PreparedSelection::Commit(PreparedDiff::build(data))
                } else {
                    PreparedSelection::Workdir(Arc::new(DiffView::build(data)))
                };
                if tx.send(PreparedReply { id, key, view }).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Self { jobs, replies }
    }

    pub fn prepare(&self, id: RepoId, key: DiffKey, data: DiffData) {
        let _ = self.jobs.send((id, key, data));
    }

    pub fn poll(&self) -> impl Iterator<Item = PreparedReply> + '_ {
        self.replies.try_iter()
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
    // Rows span the widest line so the scroll area gains a horizontal extent;
    // narrow diffs still fill the viewport.
    let content_w = gutter_w + cw * view.max_chars as f32 + 8.0;

    hvscroll("diff").show_rows(ui, row_h, view.rows.len(), |ui, range| {
        let row_w = ui.available_width().max(content_w);
        // The visible viewport, for pinning file headers while scrolled right.
        let vis = ui.clip_rect();
        for i in range {
            let (rect, _) = ui.allocate_exact_size(vec2(row_w, row_h), Sense::hover());
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
                    // Pinned to the visible viewport, not the (possibly much
                    // wider) row, so the header reads while scrolled right.
                    let left = vis.left().max(rect.left());
                    let right = vis.right().min(rect.right());
                    let r = p.text(
                        pos2(right - 6.0, y),
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
                        pos2(left + 8.0, y),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FileDiff;
    use std::time::Duration;

    fn sample() -> DiffData {
        DiffData {
            adds: 1,
            dels: 0,
            files: vec![
                FileDiff {
                    path: "a.rs".into(),
                    adds: 1,
                    dels: 0,
                    binary: false,
                    lines: vec![DiffLine {
                        kind: LineKind::Add,
                        old_no: 0,
                        new_no: 1,
                        text: "let café = 42;".into(),
                    }],
                },
                FileDiff {
                    path: "image.png".into(),
                    adds: 0,
                    dels: 0,
                    binary: true,
                    lines: vec![],
                },
            ],
        }
    }

    #[test]
    fn prepared_commit_moves_text_and_shares_selected_rows() {
        let data = sample();
        let original_text = data.files[0].lines[0].text.as_ptr();
        let prepared = PreparedDiff::build(data);
        assert_eq!((prepared.adds, prepared.dels), (1, 0));
        assert_eq!(prepared.files.len(), 2);
        let first = prepared.file_view("a.rs").unwrap();
        let second = prepared.file_view("a.rs").unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let DiffRow::Line(line, Some(spans)) = &first.rows[1] else {
            panic!("expected highlighted source line");
        };
        assert_eq!(line.text.as_ptr(), original_text);
        assert_eq!(first.max_chars, line.text.chars().count());
        assert_eq!(spans.last().unwrap().1.end, line.text.len());
        assert!(matches!(prepared.files[1].view.rows[1], DiffRow::Binary));
        assert!(prepared.file_view("missing").is_none());
    }

    #[test]
    fn binary_truncation_marker_remains_visible() {
        let mut data = sample();
        data.files[1].lines.push(DiffLine {
            kind: LineKind::Hunk,
            old_no: 0,
            new_no: 0,
            text: "… diff truncated …".into(),
        });
        let prepared = PreparedDiff::build(data);
        assert!(matches!(
            prepared.files[1].view.rows[2],
            DiffRow::Line(_, None)
        ));
    }

    #[test]
    fn background_preparation_preserves_key_and_all_workdir_files() {
        let worker = DiffWorker::spawn(eframe::egui::Context::default());
        let key = DiffKey::Workdir {
            path: "a.rs".into(),
            staged: false,
        };
        worker.prepare(7, key.clone(), sample());
        let reply = worker
            .replies
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        assert_eq!(reply.id, 7);
        assert_eq!(reply.key, key);
        let PreparedSelection::Workdir(view) = reply.view else {
            panic!("expected working tree diff");
        };
        assert_eq!(view.rows.len(), 4);
        assert_eq!((view.adds, view.dels), (1, 0));
        assert!(matches!(view.rows[3], DiffRow::Binary));
    }
}

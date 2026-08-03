//! The central CONFLICT RESOLUTION module: per-block ours/theirs/both picks
//! over a parsed conflicted file. `ConflictView::build` flattens the worker's
//! `ConflictSegment`s into render rows once -- syntax highlighting included --
//! so the per-frame body is a plain virtualized paint over the visible range,
//! exactly like `diff.rs`.

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    Align, Align2, Color32, CornerRadius, FontId, Layout, Painter, Rect, Sense, Spinner, Stroke,
    StrokeKind, TextStyle, Ui, pos2, vec2,
};

use super::highlight::{LineHighlighter, Spans};
use super::{HudButton, module, text_width, truncate_left, vscroll};
use crate::app::RepoTab;
use crate::git::{ConflictFile, ConflictSegment, Job, Resolution};
use crate::theme::{self, ACCENT, BG2, BORDER, BRIGHT, DIM, GREEN, PURPLE, RED, TEXT};

/// Indent reserved left of the code for the side bars / gutter spine.
const GUTTER_W: f32 = 16.0;

/// UI model for one conflicted file: pre-flattened, pre-highlighted rows plus
/// the user's per-block choices. Built once per `Update::Conflict` reply.
pub struct ConflictView {
    path: String,
    kind: ViewKind,
}

enum ViewKind {
    Text {
        /// Marker labels, uppercased at build time so block headers paint
        /// them without per-frame allocation.
        ours_label: String,
        theirs_label: String,
        trailing_newline: bool,
        /// One entry per `<<<<<<<` block, indexed by the rows' `block`.
        choices: Vec<Choice>,
        rows: Vec<Row>,
    },
    Binary,
    DeletedByUs,
    DeletedByThem,
}

#[derive(Clone, Copy, PartialEq)]
enum Choice {
    Unresolved,
    Ours,
    Theirs,
    Both,
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Ours,
    Theirs,
}

impl Side {
    fn color(self) -> Color32 {
        match self {
            Side::Ours => GREEN,
            Side::Theirs => PURPLE,
        }
    }
}

enum Row {
    Context {
        text: String,
        spans: Option<Spans>,
    },
    /// The block strip: "#{n}", the side labels, and the choice toggles.
    Header {
        block: usize,
    },
    Line {
        block: usize,
        side: Side,
        /// Last line of its block -- paints the closing border rule.
        last: bool,
        text: String,
        spans: Option<Spans>,
    },
}

/// Tab mutations and job pushes recorded inside the module closure (which
/// holds the `tab.conflict` borrow) and applied after layout -- the same
/// deferred-action pattern as `graph.rs`.
enum Act {
    Back,
    Resolve(Resolution),
}

impl ConflictView {
    pub fn build(path: String, file: ConflictFile) -> Self {
        let kind = match file {
            ConflictFile::Text {
                ours_label,
                theirs_label,
                segments,
                trailing_newline,
            } => {
                let mut hl = LineHighlighter::for_path(&path);
                let mut rows = Vec::new();
                let mut blocks = 0;
                for seg in segments {
                    match seg {
                        ConflictSegment::Context(lines) => {
                            for text in lines {
                                let spans = hl.spans(&text);
                                rows.push(Row::Context { text, spans });
                            }
                        }
                        ConflictSegment::Conflict { ours, theirs } => {
                            let block = blocks;
                            blocks += 1;
                            rows.push(Row::Header { block });
                            let (no, nt) = (ours.len(), theirs.len());
                            for (i, text) in ours.into_iter().enumerate() {
                                let spans = hl.spans(&text);
                                rows.push(Row::Line {
                                    block,
                                    side: Side::Ours,
                                    last: nt == 0 && i + 1 == no,
                                    text,
                                    spans,
                                });
                            }
                            for (i, text) in theirs.into_iter().enumerate() {
                                let spans = hl.spans(&text);
                                rows.push(Row::Line {
                                    block,
                                    side: Side::Theirs,
                                    last: i + 1 == nt,
                                    text,
                                    spans,
                                });
                            }
                        }
                    }
                }
                ViewKind::Text {
                    ours_label: ours_label.to_uppercase(),
                    theirs_label: theirs_label.to_uppercase(),
                    trailing_newline,
                    choices: vec![Choice::Unresolved; blocks],
                    rows,
                }
            }
            ConflictFile::Binary => ViewKind::Binary,
            ConflictFile::DeletedByUs => ViewKind::DeletedByUs,
            ConflictFile::DeletedByThem => ViewKind::DeletedByThem,
        };
        Self { path, kind }
    }
}

/// The CONFLICT RESOLUTION module. Text conflicts get per-block toggles and
/// MARK RESOLVED (enabled only once every block has a choice) assembles the
/// picked content; binary and delete-vs-modify conflicts get the two
/// whole-file resolutions instead.
pub fn show(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let readout = match &tab.conflict {
        None => "SCANNING".to_owned(),
        Some(v) => match &v.kind {
            ViewKind::Text { choices, .. } => {
                let done = choices.iter().filter(|c| **c != Choice::Unresolved).count();
                format!("{done}/{} RESOLVED", choices.len())
            }
            _ => "0/1 RESOLVED".to_owned(),
        },
    };
    let busy = tab.in_flight > 0;
    let mut act: Option<Act> = None;
    module(ui, None, "CONFLICT RESOLUTION", &readout, |ui| {
        let Some(view) = tab.conflict.as_mut() else {
            // Selection made but the worker hasn't parsed the file yet.
            ui.add_space(ui.available_height() * 0.45);
            ui.vertical_centered(|ui| ui.add(Spinner::new().size(24.0).color(ACCENT)));
            return;
        };
        match &mut view.kind {
            ViewKind::Text {
                ours_label,
                theirs_label,
                trailing_newline,
                choices,
                rows,
            } => {
                header(
                    ui,
                    &view.path,
                    busy,
                    rows,
                    choices,
                    *trailing_newline,
                    &mut act,
                );
                ui.add_space(4.0);
                body(ui, rows, choices, ours_label, theirs_label);
            }
            ViewKind::Binary => whole_file(
                ui,
                &view.path,
                "BINARY FILE — PICK A SIDE",
                "TAKE OURS",
                "TAKE THEIRS",
                busy,
                &mut act,
            ),
            ViewKind::DeletedByUs => whole_file(
                ui,
                &view.path,
                "DELETED BY US, MODIFIED BY THEM",
                "KEEP DELETED (OURS)",
                "RESTORE THEIRS",
                busy,
                &mut act,
            ),
            ViewKind::DeletedByThem => whole_file(
                ui,
                &view.path,
                "MODIFIED BY US, DELETED BY THEM",
                "KEEP OURS",
                "DELETE FILE (THEIRS)",
                busy,
                &mut act,
            ),
        }
    });
    match act {
        Some(Act::Back) => {
            tab.selection = None;
            tab.conflict = None;
        }
        Some(Act::Resolve(resolution)) => {
            if let Some(v) = &tab.conflict {
                jobs.push(Job::ResolveConflict {
                    id: tab.id,
                    path: v.path.clone(),
                    resolution,
                });
            }
        }
        None => {}
    }
}

/// Header row: CONFLICT chip + path on the left, the whole-file and per-block
/// resolution actions right-aligned (rightmost first in the RTL layout).
fn header(
    ui: &mut Ui,
    path: &str,
    busy: bool,
    rows: &[Row],
    choices: &[Choice],
    trailing_newline: bool,
    act: &mut Option<Act>,
) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add(HudButton::new("◀ GRAPH").small(true)).clicked() {
                *act = Some(Act::Back);
            }
            if ui
                .add_enabled(!busy, HudButton::new("TAKE THEIRS").small(true))
                .clicked()
            {
                *act = Some(Act::Resolve(Resolution::Theirs));
            }
            if ui
                .add_enabled(!busy, HudButton::new("TAKE OURS").small(true))
                .clicked()
            {
                *act = Some(Act::Resolve(Resolution::Ours));
            }
            let ready = choices.iter().all(|c| *c != Choice::Unresolved);
            if ui
                .add_enabled(
                    ready && !busy,
                    HudButton::new("✔ MARK RESOLVED").small(true).accent(true),
                )
                .clicked()
            {
                *act = Some(Act::Resolve(Resolution::Content(assemble(
                    rows,
                    choices,
                    trailing_newline,
                ))));
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                title_strip(ui, path);
            });
        });
    });
}

/// CONFLICT chip + left-truncated path, mirroring the diff header's chip.
fn title_strip(ui: &mut Ui, path: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    let p = ui.painter();
    let y = rect.center().y;
    let small = TextStyle::Small.resolve(ui.style());
    let body = TextStyle::Body.resolve(ui.style());

    let tag_w = text_width(ui, "CONFLICT", &small) + 12.0;
    let chip = Rect::from_min_size(pos2(rect.left(), y - 9.0), vec2(tag_w, 18.0));
    p.rect_filled(chip, CornerRadius::ZERO, theme::alpha(RED, 26));
    p.rect_stroke(
        chip,
        CornerRadius::ZERO,
        Stroke::new(1.0_f32, RED),
        StrokeKind::Inside,
    );
    p.text(chip.center(), Align2::CENTER_CENTER, "CONFLICT", small, RED);

    let text_x = chip.right() + 10.0;
    // Truncate from the left so the filename stays visible.
    let title = truncate_left(ui, path, &body, rect.right() - 6.0 - text_x);
    p.text(pos2(text_x, y), Align2::LEFT_CENTER, title, body, BRIGHT);

    theme::fade_rule(
        p,
        pos2(rect.left(), rect.bottom() + 1.0),
        rect.width() * 0.7,
        ACCENT,
    );
}

fn body(ui: &mut Ui, rows: &[Row], choices: &mut [Choice], ours_label: &str, theirs_label: &str) {
    let font = TextStyle::Monospace.resolve(ui.style());
    let small = TextStyle::Small.resolve(ui.style());
    // Same trick as diff.rs: fold the inter-item gap into the row so the
    // background tints on consecutive lines touch.
    let row_h = ui.text_style_height(&TextStyle::Monospace) + 2.0 + ui.spacing().item_spacing.y;
    ui.spacing_mut().item_spacing.y = 0.0;

    vscroll("conflict").show_rows(ui, row_h, rows.len(), |ui, range| {
        for i in range {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
            if !ui.is_rect_visible(rect) {
                continue;
            }
            match &rows[i] {
                Row::Context { text, spans } => {
                    let p = ui.painter().with_clip_rect(rect);
                    // Slim gutter spine in place of line numbers (they are
                    // ambiguous mid-conflict); the block side bars sit on
                    // the same x so it reads as one rail.
                    let gx = rect.left() + 1.0;
                    p.line_segment(
                        [pos2(gx, rect.top()), pos2(gx, rect.bottom())],
                        Stroke::new(1.0_f32, theme::alpha(BORDER, 160)),
                    );
                    paint_code(ui, &p, rect, &font, text, spans, TEXT);
                }
                Row::Header { block } => {
                    block_header(ui, rect, *block, choices, ours_label, theirs_label, &small);
                }
                Row::Line {
                    block,
                    side,
                    last,
                    text,
                    spans,
                } => {
                    let p = ui.painter().with_clip_rect(rect);
                    let kept = match choices[*block] {
                        Choice::Unresolved | Choice::Both => true,
                        Choice::Ours => *side == Side::Ours,
                        Choice::Theirs => *side == Side::Theirs,
                    };
                    let col = side.color();
                    let (bg_a, bar_a) = if kept { (26, 255) } else { (10, 90) };
                    p.rect_filled(rect, CornerRadius::ZERO, theme::alpha(col, bg_a));
                    p.rect_filled(
                        Rect::from_min_max(rect.left_top(), pos2(rect.left() + 2.0, rect.bottom())),
                        CornerRadius::ZERO,
                        theme::alpha(col, bar_a),
                    );
                    if kept {
                        paint_code(ui, &p, rect, &font, text, spans, TEXT);
                    } else {
                        // The rejected side drops its syntax colors so the
                        // kept side reads as the file's future content.
                        p.text(
                            pos2(rect.left() + GUTTER_W, rect.center().y),
                            Align2::LEFT_CENTER,
                            text,
                            font.clone(),
                            DIM,
                        );
                    }
                    if *last {
                        p.line_segment(
                            [rect.left_bottom(), rect.right_bottom()],
                            Stroke::new(1.0_f32, BORDER),
                        );
                    }
                }
            }
        }
    });
}

/// One block's strip: "#{n}" + side labels left, OURS/THEIRS/BOTH toggles
/// right. The active toggle renders filled like the graph's ref chips.
fn block_header(
    ui: &mut Ui,
    rect: Rect,
    block: usize,
    choices: &mut [Choice],
    ours_label: &str,
    theirs_label: &str,
    small: &FontId,
) {
    let p = ui.painter().with_clip_rect(rect);
    let y = rect.center().y;
    p.rect_filled(rect, CornerRadius::ZERO, BG2);
    // Opening border rule: with the last line's closing rule, the block
    // reads as one framed unit.
    p.line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.0_f32, BORDER),
    );

    let defs = [
        ("OURS", GREEN, Choice::Ours),
        ("THEIRS", PURPLE, Choice::Theirs),
        ("BOTH", ACCENT, Choice::Both),
    ];
    // Lay the chips out from the right edge first so the labels know where
    // to stop.
    let mut chips = [Rect::NOTHING; 3];
    let mut right = rect.right() - 4.0;
    for i in (0..defs.len()).rev() {
        let w = text_width(ui, defs[i].0, small) + 14.0;
        chips[i] = Rect::from_min_size(pos2(right - w, y - 8.0), vec2(w, 16.0));
        right = chips[i].left() - 6.0;
    }

    let lp = p.with_clip_rect(Rect::from_min_max(
        rect.left_top(),
        pos2(right - 4.0, rect.bottom()),
    ));
    let r = lp.text(
        pos2(rect.left() + 6.0, y),
        Align2::LEFT_CENTER,
        format!("#{}", block + 1),
        small.clone(),
        BRIGHT,
    );
    let r = lp.text(
        pos2(r.right() + 10.0, y),
        Align2::LEFT_CENTER,
        ours_label,
        small.clone(),
        GREEN,
    );
    let r = lp.text(
        pos2(r.right() + 6.0, y),
        Align2::LEFT_CENTER,
        "vs",
        small.clone(),
        DIM,
    );
    lp.text(
        pos2(r.right() + 6.0, y),
        Align2::LEFT_CENTER,
        theirs_label,
        small.clone(),
        PURPLE,
    );

    for (i, (label, color, choice)) in defs.into_iter().enumerate() {
        let chip = chips[i];
        let resp = ui.interact(chip, ui.id().with((block, i)), Sense::click());
        let active = choices[block] == choice;
        if active {
            p.rect_filled(chip, CornerRadius::ZERO, theme::alpha(color, 26));
        }
        let border = if active {
            color
        } else if resp.hovered() {
            theme::alpha(color, 160)
        } else {
            theme::alpha(DIM, 120)
        };
        p.rect_stroke(
            chip,
            CornerRadius::ZERO,
            Stroke::new(1.0_f32, border),
            StrokeKind::Inside,
        );
        let text_color = if active {
            color
        } else if resp.hovered() {
            TEXT
        } else {
            DIM
        };
        p.text(
            chip.center(),
            Align2::CENTER_CENTER,
            label,
            small.clone(),
            text_color,
        );
        if resp.clicked() {
            // Clicking the active choice disarms the block back to Unresolved.
            choices[block] = if active { Choice::Unresolved } else { choice };
        }
    }
}

/// Syntax-highlighted monospace line at the gutter indent; falls back to a
/// single-color line for unknown languages, like diff.rs.
fn paint_code(
    ui: &mut Ui,
    p: &Painter,
    rect: Rect,
    font: &FontId,
    text: &str,
    spans: &Option<Spans>,
    fallback: Color32,
) {
    let y = rect.center().y;
    match spans {
        Some(spans) if !spans.is_empty() => {
            let mut job = LayoutJob::default();
            for (color, range) in spans {
                job.append(
                    &text[range.clone()],
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
            p.galley(pos2(rect.left() + GUTTER_W, top), galley, TEXT);
        }
        _ => {
            p.text(
                pos2(rect.left() + GUTTER_W, y),
                Align2::LEFT_CENTER,
                text,
                font.clone(),
                fallback,
            );
        }
    }
}

/// Rebuild the file text from the kept rows. `Both` yields ours-then-theirs
/// because that is the order the rows were flattened in.
fn assemble(rows: &[Row], choices: &[Choice], trailing_newline: bool) -> String {
    let mut out = String::new();
    let mut any = false;
    for row in rows {
        let text = match row {
            Row::Context { text, .. } => Some(text),
            Row::Header { .. } => None,
            Row::Line {
                block, side, text, ..
            } => {
                let keep = match choices[*block] {
                    Choice::Unresolved => false,
                    Choice::Both => true,
                    Choice::Ours => *side == Side::Ours,
                    Choice::Theirs => *side == Side::Theirs,
                };
                keep.then_some(text)
            }
        };
        if let Some(t) = text {
            if any {
                out.push('\n');
            }
            out.push_str(t);
            any = true;
        }
    }
    if trailing_newline {
        out.push('\n');
    }
    out
}

/// Binary and delete-vs-modify conflicts: no blocks to pick through, so a
/// centered standby readout with the two whole-file resolutions.
fn whole_file(
    ui: &mut Ui,
    path: &str,
    msg: &str,
    ours_label: &str,
    theirs_label: &str,
    busy: bool,
    act: &mut Option<Act>,
) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add(HudButton::new("◀ GRAPH").small(true)).clicked() {
                *act = Some(Act::Back);
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                title_strip(ui, path);
            });
        });
    });

    ui.add_space((ui.available_height() * 0.5 - 70.0).max(0.0));
    let cx = ui.max_rect().center().x;
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    theme::tracked_text(
        ui.painter(),
        pos2(cx, r.center().y),
        Align2::CENTER_CENTER,
        msg,
        theme::display_font(15.0),
        DIM,
        2.0,
    );
    ui.add_space(14.0);
    ui.vertical_centered(|ui| {
        if ui
            .add_enabled(!busy, HudButton::new(ours_label).min_size(vec2(240.0, 0.0)))
            .clicked()
        {
            *act = Some(Act::Resolve(Resolution::Ours));
        }
        ui.add_space(8.0);
        if ui
            .add_enabled(
                !busy,
                HudButton::new(theirs_label).min_size(vec2(240.0, 0.0)),
            )
            .clicked()
        {
            *act = Some(Act::Resolve(Resolution::Theirs));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> ConflictView {
        ConflictView::build(
            "a.txt".into(),
            ConflictFile::Text {
                ours_label: "HEAD".into(),
                theirs_label: "feature".into(),
                segments: vec![
                    ConflictSegment::Context(vec!["fn main() {".into()]),
                    ConflictSegment::Conflict {
                        ours: vec!["    a();".into()],
                        theirs: vec!["    b();".into(), "    c();".into()],
                    },
                    ConflictSegment::Context(vec!["}".into()]),
                ],
                trailing_newline: true,
            },
        )
    }

    #[test]
    fn assembles_choice_and_skips_base() {
        let ViewKind::Text { rows, .. } = &view().kind else {
            panic!("expected text kind");
        };
        assert_eq!(
            assemble(rows, &[Choice::Ours], true),
            "fn main() {\n    a();\n}\n"
        );
        assert_eq!(
            assemble(rows, &[Choice::Theirs], false),
            "fn main() {\n    b();\n    c();\n}"
        );
        // Both = ours then theirs, in flattening order.
        assert_eq!(
            assemble(rows, &[Choice::Both], true),
            "fn main() {\n    a();\n    b();\n    c();\n}\n"
        );
    }
}

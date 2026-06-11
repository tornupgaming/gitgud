//! App chrome: command deck (top bar), readout rail (status bar), and the
//! radar empty state.

use eframe::egui::{
    Align, Align2, Layout, Rect, RichText, Sense, Shape, Spinner, Stroke, TextStyle, Ui, pos2, vec2,
};

use super::{HudButton, text_width};
use crate::app::RepoTab;
use crate::theme::{self, ACCENT, AMBER, BG1, BG2, BORDER, BRIGHT, DIM, GREEN, RED};

pub enum TopBarAction {
    Select(usize),
    Close(usize),
    Open,
    Fetch,
    Push,
}

pub fn top_bar(ui: &mut Ui, tabs: &[RepoTab], active: usize) -> Option<TopBarAction> {
    let mut action = None;
    ui.horizontal_centered(|ui| {
        logo(ui);

        for (i, tab) in tabs.iter().enumerate() {
            match tab_plate(ui, tab, i == active) {
                Some(TabClick::Select) => action = Some(TopBarAction::Select(i)),
                Some(TabClick::Close) => action = Some(TopBarAction::Close(i)),
                None => {}
            }
        }
        if ui.add(HudButton::new("+ OPEN")).clicked() {
            action = Some(TopBarAction::Open);
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Both network actions need a loaded repo; disabled until then.
            let ready = tabs.get(active).is_some_and(|t| t.loaded);
            if ui.add_enabled(ready, HudButton::new("⟳ FETCH")).clicked() {
                action = Some(TopBarAction::Fetch);
            }
            if ui.add_enabled(ready, HudButton::new("↑ PUSH")).clicked() {
                action = Some(TopBarAction::Push);
            }
            if tabs.iter().any(|t| t.in_flight > 0) {
                theme::tracked_text(
                    ui.painter(),
                    ui.cursor().right_center() - vec2(6.0, 0.0),
                    Align2::RIGHT_CENTER,
                    "SYNC",
                    theme::display_font(11.0),
                    ACCENT,
                    2.0,
                );
                ui.add_space(44.0);
                ui.add(Spinner::new().size(14.0).color(ACCENT));
            }
        });
    });

    // Accent hairline + corner ticks along the deck's lower edge.
    let bar = ui.clip_rect();
    let y = bar.bottom() - 0.5;
    let p = ui.painter();
    p.line_segment(
        [pos2(bar.left(), y), pos2(bar.right(), y)],
        Stroke::new(1.0, theme::alpha(ACCENT, 90)),
    );
    p.line_segment(
        [pos2(bar.left(), y), pos2(bar.left() + 14.0, y)],
        Stroke::new(2.0, ACCENT),
    );
    p.line_segment(
        [pos2(bar.right() - 14.0, y), pos2(bar.right(), y)],
        Stroke::new(2.0, ACCENT),
    );
    action
}

/// App emblem: double diamond + tracked wordmark + protocol subtitle.
fn logo(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(196.0, ui.available_height()), Sense::hover());
    let p = ui.painter();
    let cy = rect.center().y;
    let cx = rect.left() + 17.0;
    let diamond = |r: f32| {
        vec![
            pos2(cx, cy - r),
            pos2(cx + r, cy),
            pos2(cx, cy + r),
            pos2(cx - r, cy),
        ]
    };
    theme::glow_outline(p, diamond(12.0), theme::alpha(ACCENT, 200));
    p.add(Shape::convex_polygon(
        diamond(5.5),
        theme::alpha(ACCENT, 170),
        Stroke::NONE,
    ));
    let x = rect.left() + 40.0;
    theme::tracked_text(
        p,
        pos2(x, cy + 2.0),
        Align2::LEFT_BOTTOM,
        "GITGUD",
        theme::display_font(20.0),
        BRIGHT,
        4.0,
    );
    theme::tracked_text(
        p,
        pos2(x, cy + 3.0),
        Align2::LEFT_TOP,
        "TACTICAL GIT CONSOLE",
        theme::display_font(9.5),
        DIM,
        1.6,
    );
}

enum TabClick {
    Select,
    Close,
}

/// One repo tab: a chamfered plate with a status diamond, name, and close ✕.
fn tab_plate(ui: &mut Ui, tab: &RepoTab, is_active: bool) -> Option<TabClick> {
    let font = TextStyle::Body.resolve(ui.style());
    let name_w = text_width(ui, &tab.name, &font);
    let size = vec2(name_w + 52.0, 30.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if !ui.is_rect_visible(rect) {
        return None;
    }

    let outline = theme::chamfer(rect, 6.0);
    let p = ui.painter();
    let fill = if is_active { BG2 } else { BG1 };
    p.add(Shape::convex_polygon(outline.clone(), fill, Stroke::NONE));
    if is_active {
        theme::glow_outline(p, outline, ACCENT);
    } else {
        let color = if resp.hovered() { ACCENT } else { BORDER };
        p.add(Shape::closed_line(outline, Stroke::new(1.0, color)));
    }

    // Status diamond: amber while jobs are in flight, green when settled.
    let busy = tab.in_flight > 0;
    let (dc, cy) = (rect.left() + 12.0, rect.center().y);
    let dot = if busy { AMBER } else { GREEN };
    p.add(Shape::convex_polygon(
        vec![
            pos2(dc, cy - 3.5),
            pos2(dc + 3.5, cy),
            pos2(dc, cy + 3.5),
            pos2(dc - 3.5, cy),
        ],
        if is_active {
            dot
        } else {
            theme::alpha(dot, 130)
        },
        Stroke::NONE,
    ));

    p.text(
        pos2(rect.left() + 22.0, cy),
        Align2::LEFT_CENTER,
        &tab.name,
        font,
        if is_active { BRIGHT } else { DIM },
    );

    let close = Rect::from_center_size(pos2(rect.right() - 14.0, cy), vec2(14.0, 14.0));
    let close_resp = ui.interact(close, resp.id.with("close"), Sense::click());
    p.text(
        close.center(),
        Align2::CENTER_CENTER,
        "×",
        TextStyle::Body.resolve(ui.style()),
        if close_resp.hovered() { RED } else { DIM },
    );

    if close_resp.clicked() {
        Some(TabClick::Close)
    } else if resp.clicked() {
        Some(TabClick::Select)
    } else {
        None
    }
}

pub fn status_bar(ui: &mut Ui, tab: Option<&RepoTab>, app_log: Option<&(bool, String)>) {
    let bar = ui.max_rect();
    // Accent hairline along the rail's top edge (the busy scanline rides it).
    let top = ui.clip_rect().top() + 0.5;
    ui.painter().line_segment(
        [
            pos2(ui.clip_rect().left(), top),
            pos2(ui.clip_rect().right(), top),
        ],
        Stroke::new(1.0, theme::alpha(ACCENT, 70)),
    );

    let log = tab.and_then(|t| t.log.as_ref()).or(app_log);
    let busy = tab.is_some_and(|t| t.in_flight > 0);

    ui.horizontal_centered(|ui| {
        // LINK readout: green steady, amber while syncing, red after an error.
        let dot = match (busy, log) {
            (true, _) => AMBER,
            (false, Some((true, _))) => RED,
            _ => GREEN,
        };
        let (r, _) = ui.allocate_exact_size(vec2(46.0, ui.available_height()), Sense::hover());
        let p = ui.painter();
        p.circle_filled(pos2(r.left() + 5.0, r.center().y), 3.0, dot);
        theme::tracked_text(
            p,
            pos2(r.left() + 14.0, r.center().y),
            Align2::LEFT_CENTER,
            "LINK",
            theme::display_font(11.0),
            DIM,
            1.5,
        );

        if let Some(tab) = tab {
            // Branch marker: hollow diamond on a branch, filled when detached.
            let (r, _) = ui.allocate_exact_size(vec2(10.0, ui.available_height()), Sense::hover());
            let c = r.center();
            let pts = vec![
                pos2(c.x, c.y - 4.0),
                pos2(c.x + 4.0, c.y),
                pos2(c.x, c.y + 4.0),
                pos2(c.x - 4.0, c.y),
            ];
            if tab.head_detached {
                ui.painter()
                    .add(Shape::convex_polygon(pts, AMBER, Stroke::NONE));
            } else {
                ui.painter()
                    .add(Shape::closed_line(pts, Stroke::new(1.0, AMBER)));
            }
            ui.label(RichText::new(&tab.head).color(AMBER));
            ui.label(
                RichText::new(tab.path.display().to_string())
                    .small()
                    .color(DIM),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Per-tab log wins; the app log catches errors without a tab.
            if let Some((is_err, msg)) = log {
                let color = if *is_err { RED } else { DIM };
                ui.label(RichText::new(msg).color(color));
            }
            if busy {
                if let Some(t) = tab {
                    theme::tracked_text(
                        ui.painter(),
                        ui.cursor().right_center() - vec2(8.0, 0.0),
                        Align2::RIGHT_CENTER,
                        &format!("QUEUE {:02}", t.in_flight),
                        theme::display_font(11.0),
                        ACCENT,
                        1.5,
                    );
                    ui.add_space(70.0);
                }
            }
        });
    });

    if busy {
        // Animated scanline along the rail's top edge while jobs are in flight.
        let time = ui.input(|i| i.time);
        let seg = bar.width() * 0.18;
        let span = (bar.width() - seg).max(0.0);
        let x = bar.left() + span * (0.5 + 0.5 * (time * 2.4).sin()) as f32;
        let y = bar.top();
        ui.painter()
            .line_segment([pos2(x, y), pos2(x + seg, y)], Stroke::new(1.5, ACCENT));
        ui.ctx().request_repaint();
    }
}

/// Returns true when the "OPEN REPOSITORY" button was clicked.
pub fn empty_state(ui: &mut Ui) -> bool {
    let mut open = false;
    let area = ui.max_rect();
    theme::corner_brackets(
        ui.painter(),
        area.shrink(18.0),
        22.0,
        Stroke::new(1.5, ACCENT),
    );

    ui.add_space((ui.available_height() * 0.5 - 270.0).max(0.0));
    ui.vertical_centered(|ui| {
        radar(ui);
        ui.add_space(10.0);
        let cx = ui.max_rect().center().x;
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
        theme::tracked_text(
            ui.painter(),
            pos2(cx, r.center().y),
            Align2::CENTER_CENTER,
            "GITGUD",
            theme::display_font(32.0),
            BRIGHT,
            10.0,
        );
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
        theme::tracked_text(
            ui.painter(),
            pos2(cx, r.center().y),
            Align2::CENTER_CENTER,
            "TACTICAL GIT OPERATIONS CONSOLE",
            theme::display_font(11.5),
            DIM,
            3.0,
        );
        ui.add_space(20.0);
        open = ui
            .add(
                HudButton::new("OPEN REPOSITORY")
                    .accent(true)
                    .min_size(vec2(230.0, 34.0)),
            )
            .clicked();
        ui.add_space(10.0);
        ui.label(
            RichText::new("or drop a folder anywhere")
                .small()
                .color(DIM),
        );
    });
    open
}

/// Decorative radar sweep: concentric rings, rotating fading trail, pulsing
/// contact blips. Pure painter work; repaints continuously, but only while
/// the empty state is on screen.
fn radar(ui: &mut Ui) {
    const R: f32 = 110.0;
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), 2.0 * R + 16.0), Sense::hover());
    let c = rect.center();
    let p = ui.painter();
    let time = ui.input(|i| i.time);

    for (radius, a) in [(R, 16), (R * 0.68, 28), (R * 0.36, 44)] {
        p.circle_stroke(c, radius, Stroke::new(1.0, theme::alpha(ACCENT, a)));
    }
    // Crosshair axes.
    let axis = Stroke::new(1.0, theme::alpha(ACCENT, 22));
    p.line_segment([pos2(c.x - R, c.y), pos2(c.x + R, c.y)], axis);
    p.line_segment([pos2(c.x, c.y - R), pos2(c.x, c.y + R)], axis);

    // Sweep: a bright leading edge with a fading trail behind it.
    let angle = time * 0.9;
    const TRAIL: usize = 40;
    for i in 0..TRAIL {
        let a = angle - i as f64 * 0.03;
        let fade = 1.0 - i as f32 / TRAIL as f32;
        let (sin, cos) = a.sin_cos();
        let tip = pos2(c.x + cos as f32 * R, c.y + sin as f32 * R);
        let w = if i == 0 { 1.5 } else { 1.0 };
        p.line_segment(
            [c, tip],
            Stroke::new(w, theme::alpha(ACCENT, (fade * 90.0) as u8)),
        );
    }

    // Contact blips, pulsing out of phase.
    for (i, (dx, dy, color)) in [
        (0.55, -0.3, GREEN),
        (-0.4, 0.5, AMBER),
        (-0.6, -0.55, ACCENT),
    ]
    .iter()
    .enumerate()
    {
        let pulse = ((time * 1.4 + i as f64 * 2.1).sin() * 0.5 + 0.5) as f32;
        let pos = pos2(c.x + dx * R, c.y + dy * R);
        p.circle_filled(pos, 2.5, theme::alpha(*color, 60 + (pulse * 170.0) as u8));
        p.circle_stroke(
            pos,
            3.0 + pulse * 5.0,
            Stroke::new(1.0, theme::alpha(*color, ((1.0 - pulse) * 120.0) as u8)),
        );
    }

    // Center emblem.
    p.add(Shape::convex_polygon(
        vec![
            pos2(c.x, c.y - 6.0),
            pos2(c.x + 6.0, c.y),
            pos2(c.x, c.y + 6.0),
            pos2(c.x - 6.0, c.y),
        ],
        theme::alpha(ACCENT, 180),
        Stroke::NONE,
    ));

    ui.ctx().request_repaint();
}

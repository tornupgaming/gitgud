//! Tactical-HUD theme: a proportional UI font with monospace reserved for
//! code (the diff body), sharp corners, 1px borders, cyan glow on hover.
//! All UI modules pull their colors from these consts.

use eframe::egui::{
    self, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Shape, Stroke, TextStyle, pos2,
};

pub const BG0: Color32 = Color32::from_rgb(0x07, 0x0b, 0x12);
pub const BG1: Color32 = Color32::from_rgb(0x0c, 0x12, 0x1d);
pub const BG2: Color32 = Color32::from_rgb(0x13, 0x1b, 0x2a);
pub const BG3: Color32 = Color32::from_rgb(0x1b, 0x26, 0x38);
pub const BORDER: Color32 = Color32::from_rgb(0x20, 0x30, 0x4a);

pub const TEXT: Color32 = Color32::from_rgb(0xb8, 0xc7, 0xd9);
pub const DIM: Color32 = Color32::from_rgb(0x5d, 0x72, 0x90);

pub const ACCENT: Color32 = Color32::from_rgb(0x35, 0xe0, 0xff);
pub const AMBER: Color32 = Color32::from_rgb(0xff, 0xb4, 0x54);
pub const GREEN: Color32 = Color32::from_rgb(0x5a, 0xf7, 0x8e);
pub const RED: Color32 = Color32::from_rgb(0xff, 0x5c, 0x57);
pub const PURPLE: Color32 = Color32::from_rgb(0xb5, 0x8c, 0xff);

/// `color` at the given alpha -- used for translucent diff/selection fills.
pub fn alpha(color: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

// --- Sci-fi HUD primitives ----------------------------------------------
//
// One shared shape language: every plate (buttons, panels, modals) traces a
// rectangle with two opposite corners sliced off, framed with L-shaped corner
// ticks and fading accent rules. These are plain painter helpers so any widget
// can reach for them.

/// The 6-point outline of `rect` with its top-right and bottom-left corners
/// chamfered (cut at 45° by `c`). The silhouette the whole UI is built from.
pub fn chamfer(rect: Rect, c: f32) -> Vec<Pos2> {
    let (l, t, r, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    vec![
        pos2(l, t),
        pos2(r - c, t),
        pos2(r, t + c),
        pos2(r, b),
        pos2(l + c, b),
        pos2(l, b - c),
    ]
}

/// Fill and outline a chamfered plate in one call.
pub fn plate(painter: &Painter, rect: Rect, c: f32, fill: Color32, stroke: Stroke) {
    painter.add(Shape::convex_polygon(chamfer(rect, c), fill, stroke));
}

/// L-shaped tick at each of the four corners of `rect` -- the signature HUD
/// "frame corner". `len` is the arm length of each tick.
pub fn corner_brackets(painter: &Painter, rect: Rect, len: f32, stroke: Stroke) {
    let (tl, tr) = (rect.left_top(), rect.right_top());
    let (bl, br) = (rect.left_bottom(), rect.right_bottom());
    for (corner, dx, dy) in [
        (tl, 1.0, 1.0),
        (tr, -1.0, 1.0),
        (bl, 1.0, -1.0),
        (br, -1.0, -1.0),
    ] {
        painter.line_segment([corner, pos2(corner.x + dx * len, corner.y)], stroke);
        painter.line_segment([corner, pos2(corner.x, corner.y + dy * len)], stroke);
    }
}

/// A horizontal rule starting at `start` that fades `color` out to nothing
/// over `len` points -- used under section headers and beside titles.
pub fn fade_rule(painter: &Painter, start: Pos2, len: f32, color: Color32) {
    if len <= 0.0 {
        return;
    }
    const SEGS: usize = 28;
    let seg = len / SEGS as f32;
    for i in 0..SEGS {
        let fade = 1.0 - i as f32 / SEGS as f32;
        let x = start.x + seg * i as f32;
        let stroke = Stroke::new(1.0, alpha(color, (fade * 200.0) as u8));
        painter.line_segment([pos2(x, start.y), pos2(x + seg, start.y)], stroke);
    }
}

/// Tactical backdrop for the big "void" areas (empty state, etc.): a dark base
/// washed with a faint cyan grid. Cheap -- a couple dozen lines, no per-pixel
/// work -- and drawn behind content so it never touches text legibility.
pub fn backdrop(painter: &Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, BG0);
    let step = 30.0;
    let stroke = Stroke::new(1.0, alpha(ACCENT, 10));
    let mut x = rect.left() + step;
    while x < rect.right() {
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], stroke);
        x += step;
    }
    let mut y = rect.top() + step;
    while y < rect.bottom() {
        painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], stroke);
        y += step;
    }
}

pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.global_style()).clone();

    // Proportional for all chrome/labels; Monospace stays a fixed-width font
    // and is resolved explicitly by the diff viewer, where column alignment
    // and line-number gutters depend on every glyph having the same width.
    style.text_styles = [
        (TextStyle::Heading, FontId::proportional(17.0)),
        (TextStyle::Body, FontId::proportional(14.0)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
        (TextStyle::Button, FontId::proportional(14.0)),
        (TextStyle::Small, FontId::proportional(11.0)),
    ]
    .into();

    // Slim, non-floating scrollbars -- a thin track that reads as part of the
    // HUD chrome rather than an OS overlay.
    let sc = &mut style.spacing.scroll;
    sc.floating = false;
    sc.bar_width = 9.0;
    sc.bar_inner_margin = 2.0;

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.panel_fill = BG1;
    v.window_fill = BG1;
    v.extreme_bg_color = BG0;
    // Row stripe: halfway between panel and widget background.
    v.faint_bg_color = Color32::from_rgb(0x0f, 0x16, 0x23);
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::ZERO;
    v.menu_corner_radius = CornerRadius::ZERO;
    v.selection.bg_fill = alpha(ACCENT, 45);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;

    let w = &mut v.widgets;
    for wv in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        wv.corner_radius = CornerRadius::ZERO;
        wv.expansion = 0.0;
    }
    w.noninteractive.bg_fill = BG1;
    w.noninteractive.weak_bg_fill = BG1;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);

    w.inactive.bg_fill = BG2;
    w.inactive.weak_bg_fill = BG2;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);

    // The "glow": hovering swaps the border to accent cyan.
    w.hovered.bg_fill = BG3;
    w.hovered.weak_bg_fill = BG3;
    w.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    w.hovered.fg_stroke = Stroke::new(1.0, Color32::from_rgb(0xd9, 0xe6, 0xf5));

    w.active.bg_fill = BG3;
    w.active.weak_bg_fill = BG3;
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.active.fg_stroke = Stroke::new(1.0, ACCENT);

    w.open.bg_fill = BG2;
    w.open.weak_bg_fill = BG2;
    w.open.bg_stroke = Stroke::new(1.0, BORDER);
    w.open.fg_stroke = Stroke::new(1.0, TEXT);

    ctx.set_global_style(style);
}

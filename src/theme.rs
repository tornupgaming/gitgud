//! Sci-fi HUD theme: Rajdhani display font, chamfered glowing plates over a
//! gridded void, CRT scanlines. All UI modules pull their colors and shape
//! primitives from here -- new colors join the palette below, never inline
//! at a call site.

use std::sync::Arc;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId,
    Painter, Pos2, Rect, Shadow, Shape, Stroke, TextStyle,
    epaint::{Mesh, Vertex, WHITE_UV},
    pos2, vec2,
};

/// Deepest layer -- the void behind the floating modules.
pub const BG0: Color32 = Color32::from_rgb(0x04, 0x07, 0x0d);
/// Plate/panel base fill.
pub const BG1: Color32 = Color32::from_rgb(0x0a, 0x10, 0x1b);
/// Raised elements: widget fills, title bands, section strips.
pub const BG2: Color32 = Color32::from_rgb(0x10, 0x18, 0x28);
/// Hover fill.
pub const BG3: Color32 = Color32::from_rgb(0x1a, 0x26, 0x3a);
/// Resting border; hover swaps it for [`ACCENT`].
pub const BORDER: Color32 = Color32::from_rgb(0x1e, 0x32, 0x52);

/// Body text.
pub const TEXT: Color32 = Color32::from_rgb(0xbf, 0xd2, 0xe8);
/// Emphasis: titles, selected rows, the lit button label.
pub const BRIGHT: Color32 = Color32::from_rgb(0xe6, 0xf4, 0xff);
/// Secondary: metadata, hints, disabled states.
pub const DIM: Color32 = Color32::from_rgb(0x52, 0x6a, 0x8c);

/// The signature HUD cyan: borders, glows, selection, primary actions.
pub const ACCENT: Color32 = Color32::from_rgb(0x35, 0xe0, 0xff);
/// Caution and "current": HEAD branch, busy indicators, merge state.
pub const AMBER: Color32 = Color32::from_rgb(0xff, 0xb4, 0x54);
/// Additions, ahead counts, settled status.
pub const GREEN: Color32 = Color32::from_rgb(0x5a, 0xf7, 0x8e);
/// Deletions, errors, conflicts, destructive confirms.
pub const RED: Color32 = Color32::from_rgb(0xff, 0x5c, 0x57);
/// Remote refs and the "theirs" side of conflicts.
pub const PURPLE: Color32 = Color32::from_rgb(0xb5, 0x8c, 0xff);

/// Node colors for the commit graph -- picked per commit by hashing its id so
/// the rail reads like the multi-lane graphs in game HUDs.
pub const LANES: [Color32; 5] = [
    ACCENT,
    GREEN,
    AMBER,
    PURPLE,
    Color32::from_rgb(0xff, 0x6b, 0xc5),
];

/// `color` at the given alpha -- used for translucent fills and glows.
pub fn alpha(color: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

// --- Fonts ---------------------------------------------------------------

/// Custom font family for headings/titles (Rajdhani SemiBold). Body text uses
/// the default Proportional family, which `install_fonts` points at Rajdhani
/// Medium.
const DISPLAY_FAMILY: &str = "display";

/// Rajdhani SemiBold at `size` -- headings and tracked HUD labels.
pub fn display_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(DISPLAY_FAMILY.into()))
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "rajdhani-medium".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/Rajdhani-Medium.ttf"
        ))),
    );
    fonts.font_data.insert(
        "rajdhani-semibold".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/Rajdhani-SemiBold.ttf"
        ))),
    );
    // A trimmed DejaVu Sans subset carrying just the symbol glyphs we paint as
    // icons (arrows, triangles, check/cross, warning, reload). egui's bundled
    // fonts cover only some of these, so labels like "↑ PUSH", "↓ PULL", the
    // ahead/behind chips, "▼ LOAD MORE" and the progress check/cross rendered
    // as tofu boxes. Appended LAST in every family so it only fills the gaps --
    // glyphs the earlier fonts already cover are untouched.
    fonts.font_data.insert(
        "icons".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/GitGudIcons.ttf"
        ))),
    );
    // Rajdhani first, then egui's defaults, then our icon subset as the final
    // fallback for the symbol glyphs (arrows, geometric shapes) the others lack.
    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    prop.insert(0, "rajdhani-medium".to_owned());
    prop.push("icons".to_owned());
    let mut display = prop.clone();
    display[0] = "rajdhani-semibold".to_owned();
    fonts
        .families
        .insert(FontFamily::Name(DISPLAY_FAMILY.into()), display);
    // Monospace is left alone: the diff viewer's column and gutter math assumes
    // every monospace glyph shares one width, which a proportional fallback
    // would break. The icon glyphs only appear in proportional UI chrome.
    ctx.set_fonts(fonts);
}

// --- Sci-fi HUD primitives ----------------------------------------------
//
// One shared shape language: every plate (buttons, panels, modals) traces a
// rectangle with two opposite corners sliced off, framed with L-shaped corner
// ticks, glow outlines, and fading accent rules. Plain painter helpers so any
// widget can reach for them.

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

/// Fill a convex polygon with a vertical `top` -> `bottom` gradient. egui has
/// no gradient shape, so this triangulates the polygon as a fan and lets the
/// GPU interpolate per-vertex colors; the color is linear in y, so any
/// triangulation reproduces the same gradient.
pub fn gradient_fill(painter: &Painter, points: &[Pos2], top: Color32, bottom: Color32) {
    if points.len() < 3 {
        return;
    }
    let (min_y, max_y) = points
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), p| {
            (lo.min(p.y), hi.max(p.y))
        });
    let h = (max_y - min_y).max(1.0);
    let mut mesh = Mesh::default();
    for p in points {
        mesh.vertices.push(Vertex {
            pos: *p,
            uv: WHITE_UV,
            color: top.lerp_to_gamma(bottom, (p.y - min_y) / h),
        });
    }
    for i in 1..points.len() as u32 - 1 {
        mesh.indices.extend([0, i, i + 1]);
    }
    painter.add(Shape::mesh(mesh));
}

/// Neon edge: a crisp 1px line over two widening translucent passes. egui has
/// no blur, so stacked low-alpha strokes fake the bloom.
pub fn glow_outline(painter: &Painter, points: Vec<Pos2>, color: Color32) {
    glow_passes(painter, &points, color, 1.0);
    painter.add(Shape::closed_line(points, Stroke::new(1.0_f32, color)));
}

/// Just the translucent bloom passes of [`glow_outline`], scaled by `strength`
/// (0..=1) so animated widgets can fade the glow in and out.
pub fn glow_passes(painter: &Painter, points: &[Pos2], color: Color32, strength: f32) {
    for (w, a) in [(5.0_f32, 16.0), (2.5_f32, 44.0)] {
        painter.add(Shape::closed_line(
            points.to_vec(),
            Stroke::new(w, alpha(color, (a * strength) as u8)),
        ));
    }
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
        let stroke = Stroke::new(1.0_f32, alpha(color, (fade * 200.0) as u8));
        painter.line_segment([pos2(x, start.y), pos2(x + seg, start.y)], stroke);
    }
}

/// Letter-spaced text, the HUD-typography workhorse. egui has no tracking, so
/// each glyph is laid out and drawn individually. Returns the painted rect.
/// Only used for short labels (titles, buttons), so the per-glyph galleys are
/// cheap.
pub fn tracked_text(
    painter: &Painter,
    pos: Pos2,
    anchor: Align2,
    text: &str,
    font: FontId,
    color: Color32,
    tracking: f32,
) -> Rect {
    let galleys: Vec<_> = text
        .chars()
        .map(|ch| painter.layout_no_wrap(ch.to_string(), font.clone(), color))
        .collect();
    let width: f32 = galleys.iter().map(|g| g.size().x).sum::<f32>()
        + tracking * galleys.len().saturating_sub(1) as f32;
    let height = galleys.iter().map(|g| g.size().y).fold(0.0, f32::max);
    let min_x = match anchor.x() {
        Align::Min => pos.x,
        Align::Center => pos.x - width / 2.0,
        Align::Max => pos.x - width,
    };
    let min_y = match anchor.y() {
        Align::Min => pos.y,
        Align::Center => pos.y - height / 2.0,
        Align::Max => pos.y - height,
    };
    let mut x = min_x;
    for g in galleys {
        let w = g.size().x;
        painter.galley(pos2(x, min_y), g, color);
        x += w + tracking;
    }
    Rect::from_min_size(pos2(min_x, min_y), vec2(width, height))
}

/// Tactical backdrop for the void behind the floating modules: near-black base
/// washed with a faint cyan grid, every fourth line slightly brighter. Cheap --
/// a few dozen lines, no per-pixel work.
pub fn backdrop(painter: &Painter, rect: Rect) {
    painter.rect_filled(rect, CornerRadius::ZERO, BG0);
    let step = 26.0;
    let mut i = 0;
    let mut x = rect.left() + step;
    while x < rect.right() {
        let a = if i % 4 == 0 { 16 } else { 7 };
        painter.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.0_f32, alpha(ACCENT, a)),
        );
        x += step;
        i += 1;
    }
    i = 0;
    let mut y = rect.top() + step;
    while y < rect.bottom() {
        let a = if i % 4 == 0 { 16 } else { 7 };
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0_f32, alpha(ACCENT, a)),
        );
        y += step;
        i += 1;
    }
}

/// CRT scanlines: a 1px darkening line every 3pt, drawn over the whole frame.
/// Alpha is low enough to read as texture, not stripes.
pub fn scanlines(painter: &Painter, rect: Rect) {
    let stroke = Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(0, 0, 0, 26));
    let mut y = rect.top();
    while y < rect.bottom() {
        painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], stroke);
        y += 3.0;
    }
}

/// Install the fonts and the global egui style. Called once at startup;
/// every stock widget inherits the HUD look from this.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut style = (*ctx.global_style()).clone();

    // Rajdhani for all chrome/labels; Monospace stays a fixed-width font and
    // is resolved explicitly by the diff viewer, where column alignment and
    // line-number gutters depend on every glyph having the same width.
    style.text_styles = [
        (TextStyle::Heading, display_font(20.0)),
        (TextStyle::Body, FontId::proportional(15.0)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
        (TextStyle::Button, FontId::proportional(15.0)),
        (TextStyle::Small, FontId::proportional(12.0)),
    ]
    .into();

    // Slim, non-floating scrollbars -- a thin track that reads as part of the
    // HUD chrome rather than an OS overlay.
    let sc = &mut style.spacing.scroll;
    sc.floating = false;
    sc.bar_width = 8.0;
    sc.bar_inner_margin = 2.0;

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = BG1;
    v.extreme_bg_color = BG0;
    // Row stripe: halfway between panel and widget background.
    v.faint_bg_color = Color32::from_rgb(0x0d, 0x14, 0x21);
    v.window_stroke = Stroke::new(1.0_f32, ACCENT);
    v.window_corner_radius = CornerRadius::ZERO;
    v.menu_corner_radius = CornerRadius::ZERO;
    // Cyan bloom instead of drop shadows: dialogs glow, they don't float.
    v.window_shadow = Shadow {
        offset: [0, 0],
        blur: 24,
        spread: 2,
        color: alpha(ACCENT, 45),
    };
    v.popup_shadow = Shadow {
        offset: [0, 0],
        blur: 12,
        spread: 1,
        color: alpha(ACCENT, 35),
    };
    v.selection.bg_fill = alpha(ACCENT, 45);
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
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
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);

    w.inactive.bg_fill = BG2;
    w.inactive.weak_bg_fill = BG2;
    w.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);

    // The "glow": hovering swaps the border to accent cyan.
    w.hovered.bg_fill = BG3;
    w.hovered.weak_bg_fill = BG3;
    w.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    w.hovered.fg_stroke = Stroke::new(1.0_f32, BRIGHT);

    w.active.bg_fill = BG3;
    w.active.weak_bg_fill = BG3;
    w.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    w.active.fg_stroke = Stroke::new(1.0_f32, ACCENT);

    w.open.bg_fill = BG2;
    w.open.weak_bg_fill = BG2;
    w.open.bg_stroke = Stroke::new(1.0_f32, BORDER);
    w.open.fg_stroke = Stroke::new(1.0_f32, TEXT);

    ctx.set_global_style(style);
}

//! Tactical-HUD theme: monospace everywhere, sharp corners, 1px borders,
//! cyan glow on hover. All UI modules pull their colors from these consts.

use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle};

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

pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.global_style()).clone();

    style.text_styles = [
        (TextStyle::Heading, FontId::monospace(17.0)),
        (TextStyle::Body, FontId::monospace(13.0)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
        (TextStyle::Button, FontId::monospace(13.0)),
        (TextStyle::Small, FontId::monospace(11.0)),
    ]
    .into();

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

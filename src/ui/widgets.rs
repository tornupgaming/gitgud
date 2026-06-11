//! `HudButton` -- the app's single stylised button.
//!
//! An angular HUD "plate": the top-right and bottom-left corners are chamfered
//! (cut at 45°), a faint diagonal hatch textures the left side, the label is
//! letter-spaced caps, and the border lights up cyan with a bloom when you
//! hover. The `accent` variant is the always-lit primary action (COMMIT, the
//! modal confirm). Every clickable action in the app routes through this so
//! the look stays consistent.
//!
//! It implements [`egui::Widget`], so it drops into the usual call sites:
//! `ui.add(HudButton::new("FETCH"))` or `ui.add_enabled(ready, HudButton::…)`.

use eframe::egui::{
    Align2, Color32, Rect, Response, Sense, Shape, Stroke, TextStyle, Ui, Vec2, Widget,
    emath::easing, lerp, pos2, vec2,
};

use super::text_width;
use crate::theme::{self, ACCENT, BG1, BG2, BG3, BORDER, BRIGHT, DIM, TEXT};

/// Letter-spacing applied to every button label.
const TRACKING: f32 = 1.2;

pub struct HudButton {
    text: String,
    /// Forced label colour. `None` means "follow the hover/disabled state".
    text_color: Option<Color32>,
    /// Stretch to at least this size -- used for full-width buttons.
    min_size: Vec2,
    /// Smaller font + no hatch, for inline/secondary actions.
    small: bool,
    /// Always-lit primary action: accent fill, accent border, constant glow.
    accent: bool,
    /// Dim keyboard-shortcut hint drawn at the right edge (e.g. "CTRL+ENTER").
    hint: Option<String>,
}

impl HudButton {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            text_color: None,
            min_size: Vec2::ZERO,
            small: false,
            accent: false,
            hint: None,
        }
    }

    /// Force the label colour (otherwise it tracks the hover/disabled state).
    pub fn text_color(mut self, color: Color32) -> Self {
        self.text_color = Some(color);
        self
    }

    /// Stretch the button to at least this size (e.g. full panel width).
    pub fn min_size(mut self, size: Vec2) -> Self {
        self.min_size = size;
        self
    }

    /// Use the smaller label font, for compact secondary actions.
    pub fn small(mut self, small: bool) -> Self {
        self.small = small;
        self
    }

    /// Primary-action styling: accent fill and a constant glow.
    pub fn accent(mut self, accent: bool) -> Self {
        self.accent = accent;
        self
    }

    /// Keyboard-shortcut hint, right-aligned inside the plate.
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl Widget for HudButton {
    fn ui(self, ui: &mut Ui) -> Response {
        let text_style = if self.small {
            TextStyle::Small
        } else {
            TextStyle::Button
        };
        let font = text_style.resolve(ui.style());
        let text_h = ui.text_style_height(&text_style);
        let label_w = text_width(ui, &self.text, &font)
            + TRACKING * self.text.chars().count().saturating_sub(1) as f32;

        // Inner padding, plus extra room on the right so the chamfer never bites
        // into the label.
        let pad = if self.small {
            vec2(10.0, 3.0)
        } else {
            vec2(16.0, 6.0)
        };
        let chamfer = (text_h * 0.5 + pad.y).min(14.0);
        let desired = vec2(label_w + pad.x * 2.0 + chamfer, text_h + pad.y * 2.0);
        let size = desired.max(self.min_size);

        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        if !ui.is_rect_visible(rect) {
            return resp;
        }

        // `add_enabled(false, …)` disables the surrounding Ui; read that to pick
        // the dimmed look. `hot` = the lit, cyan-framed state.
        let enabled = ui.is_enabled();
        let hot = enabled && resp.hovered();
        let lit = self.accent && enabled;

        // Hover factor, eased 0..=1. Everything below (fill, border, glow,
        // brackets, sweep) is driven off this one value, so the whole state
        // change reads as a single 150ms motion instead of a binary flip.
        // egui keeps requesting repaints while the value is mid-flight.
        let t = ui
            .ctx()
            .animate_bool_with_time_and_easing(resp.id, hot, 0.15, easing::cubic_out);

        // Cut the top-right and bottom-left corners so the tile reads as an
        // angular HUD plate -- the same silhouette panels and modals use.
        let c = chamfer;
        let (l, top, b) = (rect.left(), rect.top(), rect.bottom());
        let outline = theme::chamfer(rect, c);

        let fill = if enabled {
            BG2.lerp_to_gamma(BG3, t)
        } else {
            BG1
        };

        let painter = ui.painter();
        painter.add(Shape::convex_polygon(outline.clone(), fill, Stroke::NONE));
        if lit {
            // Accent wash over the base fill so the plate reads as energised.
            painter.add(Shape::convex_polygon(
                outline.clone(),
                theme::alpha(ACCENT, 28 + (22.0 * t) as u8),
                Stroke::NONE,
            ));
        }
        if resp.is_pointer_button_down_on() && enabled {
            // Press flash: an extra accent wash for as long as the button is held.
            painter.add(Shape::convex_polygon(
                outline.clone(),
                theme::alpha(ACCENT, 38),
                Stroke::NONE,
            ));
        }

        // Bloom fades in with the hover factor; accent buttons glow constantly.
        let glow = if lit { 1.0 } else { t };
        if glow > 0.0 {
            theme::glow_passes(painter, &outline, ACCENT, glow);
        }
        let border_color = if !enabled {
            theme::alpha(BORDER, 120)
        } else if lit {
            ACCENT
        } else {
            BORDER.lerp_to_gamma(ACCENT, t)
        };
        painter.add(Shape::closed_line(outline, Stroke::new(1.0, border_color)));

        // Target-lock brackets: four L-shaped corner ticks that start outside
        // the plate and converge onto it as the hover factor rises, arms
        // growing as they close in.
        if t > 0.0 {
            let reach = 2.0 + 4.0 * (1.0 - t);
            let arm = (if self.small { 5.0 } else { 7.0 }) * t;
            theme::corner_brackets(
                painter,
                rect.expand(reach),
                arm,
                Stroke::new(1.5, theme::alpha(ACCENT, (230.0 * t) as u8)),
            );
        }

        // Light sweep: a slanted band that crosses the plate during the
        // transition. The sin envelope is zero at both ends of the animation,
        // so the steady states stay clean and un-hovering plays it in reverse.
        if enabled && t > 0.0 && t < 1.0 {
            let flash = (t * std::f32::consts::PI).sin();
            let h = rect.height();
            // Same 45° slant as the hatch; travels left edge to right edge.
            let x = lerp((l - h)..=rect.right(), t);
            let p = painter.with_clip_rect(rect);
            for (w, a) in [(10.0, 28.0), (3.0, 90.0)] {
                p.line_segment(
                    [pos2(x, b), pos2(x + h, top)],
                    Stroke::new(w, theme::alpha(BRIGHT, (a * flash) as u8)),
                );
            }
        }

        // Faint diagonal hatch on the left, echoing the reference art. Clipped
        // to a band so the 45° strokes never poke past the chamfered corners.
        if !self.small {
            let band_w = (rect.width() * 0.4).min(64.0);
            let band = Rect::from_min_max(pos2(l + c, top + 2.0), pos2(l + c + band_w, b - 2.0));
            let hp = ui.painter().with_clip_rect(band);
            let hatch_color = if lit {
                ACCENT
            } else {
                BORDER.lerp_to_gamma(ACCENT, t)
            };
            let hatch = Stroke::new(1.0, theme::alpha(hatch_color, 26));
            let h = band.height();
            let mut x = band.left() - h;
            while x < band.right() {
                hp.line_segment([pos2(x, band.bottom()), pos2(x + h, band.top())], hatch);
                x += 6.0;
            }
        }

        let text_color = self.text_color.unwrap_or(if !enabled {
            DIM
        } else if lit {
            BRIGHT
        } else {
            TEXT.lerp_to_gamma(BRIGHT, t)
        });
        theme::tracked_text(
            ui.painter(),
            rect.center(),
            Align2::CENTER_CENTER,
            &self.text,
            font,
            text_color,
            TRACKING,
        );
        if let Some(hint) = &self.hint {
            theme::tracked_text(
                ui.painter(),
                pos2(rect.right() - c - 6.0, rect.center().y),
                Align2::RIGHT_CENTER,
                hint,
                theme::display_font(10.0),
                if enabled {
                    theme::alpha(ACCENT, 170)
                } else {
                    DIM
                },
                1.0,
            );
        }

        resp
    }
}

//! `HudButton` -- the app's single stylised button.
//!
//! It's an angular HUD "plate": the top-right and bottom-left corners are
//! chamfered (cut at 45°), a faint diagonal hatch textures the left side, and
//! the 1px border lights up cyan when you hover -- the same "glow" idea the
//! rest of the theme uses, just made into a reusable widget. Every clickable
//! action in the app routes through this so the look stays consistent.
//!
//! It implements [`egui::Widget`], so it drops into the usual call sites:
//! `ui.add(HudButton::new("FETCH"))` or `ui.add_enabled(ready, HudButton::…)`.

use eframe::egui::{
    Align2, Color32, Rect, Response, Sense, Shape, Stroke, TextStyle, Ui, Vec2, Widget, pos2, vec2,
};

use super::text_width;
use crate::theme::{self, ACCENT, BG1, BG2, BG3, BORDER, DIM, TEXT};

pub struct HudButton {
    text: String,
    /// Forced label colour. `None` means "follow the hover/disabled state".
    text_color: Option<Color32>,
    /// Stretch to at least this size -- used for full-width buttons.
    min_size: Vec2,
    /// Smaller font + no hatch, for inline/secondary actions.
    small: bool,
}

impl HudButton {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            text_color: None,
            min_size: Vec2::ZERO,
            small: false,
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
        let label_w = text_width(ui, &self.text, &font);

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

        // Cut the top-right and bottom-left corners so the tile reads as an
        // angular HUD plate -- the same silhouette panels and modals use.
        let c = chamfer;
        let (l, t, b) = (rect.left(), rect.top(), rect.bottom());

        let fill = if !enabled {
            BG1
        } else if hot {
            BG3
        } else {
            BG2
        };
        let border = if hot { ACCENT } else { BORDER };

        let painter = ui.painter();

        // Soft "glow": a wide, translucent accent outline behind the crisp
        // border. egui has no blur, so a fat low-alpha stroke fakes it.
        if hot {
            painter.add(Shape::closed_line(
                theme::chamfer(rect, c),
                Stroke::new(4.0, theme::alpha(ACCENT, 40)),
            ));
        }

        theme::plate(painter, rect, c, fill, Stroke::new(1.0, border));

        // Faint diagonal hatch on the left, echoing the reference art. Clipped
        // to a band so the 45° strokes never poke past the chamfered corners.
        if !self.small {
            let band_w = (rect.width() * 0.4).min(64.0);
            let band = Rect::from_min_max(pos2(l + c, t + 2.0), pos2(l + c + band_w, b - 2.0));
            let hp = ui.painter().with_clip_rect(band);
            let hatch = Stroke::new(1.0, theme::alpha(if hot { ACCENT } else { BORDER }, 26));
            let h = band.height();
            let mut x = band.left() - h;
            while x < band.right() {
                hp.line_segment([pos2(x, band.bottom()), pos2(x + h, band.top())], hatch);
                x += 6.0;
            }
        }

        let text_color = self.text_color.unwrap_or(if !enabled {
            DIM
        } else if hot {
            Color32::from_rgb(0xd9, 0xe6, 0xf5)
        } else {
            TEXT
        });
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            &self.text,
            font,
            text_color,
        );

        resp
    }
}

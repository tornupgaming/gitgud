//! Shared UI chrome: the floating HUD module frame and text helpers.

pub mod conflict;
pub mod diff;
pub mod graph;
mod highlight;
pub mod progress;
mod reveal;
pub mod shell;
pub mod sidebar;
pub mod submodules;
pub mod sync;
pub mod widgets;
pub mod worktree;

pub(crate) use reveal::{factor, hud_menu, hud_modal};
pub use shell::{TopBarAction, empty_state, status_bar, top_bar};
pub use widgets::HudButton;

use eframe::egui::{Align2, FontId, Layout, Rect, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

use crate::theme::{self, ACCENT, BG0, BG1, BG2, BG3, DIM};

/// Chamfer depth shared by every module plate.
const MODULE_CHAMFER: f32 = 10.0;
/// Height of a module's title band.
const BAND_H: f32 = 26.0;

/// A floating HUD module: chamfered plate, glowing border, title band with
/// letter-spaced caps and a right-aligned readout, content below. Every panel
/// in the app is one of these, floating over the gridded void.
///
/// `height: None` fills all remaining vertical space.
pub(crate) fn module<R>(
    ui: &mut Ui,
    height: Option<f32>,
    title: &str,
    readout: &str,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    let h = height.unwrap_or_else(|| ui.available_height());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let c = MODULE_CHAMFER;
    let p = ui.painter();

    // Plate fill: lifted toward BG2 at the top, sinking toward the void color
    // at the bottom, so the module reads as lit from above instead of flat.
    theme::gradient_fill(
        p,
        &theme::chamfer(rect, c),
        BG1.lerp_to_gamma(BG2, 0.4),
        BG1.lerp_to_gamma(BG0, 0.55),
    );

    // Title band, with its top-right corner following the plate's chamfer.
    // Its own top-lit sheen makes it read as a raised strip on the plate.
    let (l, t, r) = (rect.left(), rect.top(), rect.right());
    let band_bottom = t + BAND_H;
    theme::gradient_fill(
        p,
        &[
            pos2(l, t),
            pos2(r - c, t),
            pos2(r, t + c),
            pos2(r, band_bottom),
            pos2(l, band_bottom),
        ],
        BG2.lerp_to_gamma(BG3, 0.5),
        BG2,
    );
    let title_rect = theme::tracked_text(
        p,
        pos2(l + 12.0, t + BAND_H / 2.0),
        Align2::LEFT_CENTER,
        title,
        theme::display_font(12.5),
        ACCENT,
        2.0,
    );
    theme::tracked_text(
        p,
        pos2(r - 14.0, t + BAND_H / 2.0),
        Align2::RIGHT_CENTER,
        readout,
        theme::display_font(11.0),
        DIM,
        1.5,
    );
    // Accent underline: bright over the title, fading toward the right edge.
    let y = band_bottom + 0.5;
    p.line_segment(
        [pos2(l, y), pos2(title_rect.right() + 6.0, y)],
        Stroke::new(1.0, ACCENT),
    );
    theme::fade_rule(
        p,
        pos2(title_rect.right() + 6.0, y),
        (r - title_rect.right() - 6.0).max(0.0),
        ACCENT,
    );

    theme::glow_outline(p, theme::chamfer(rect, c), theme::alpha(ACCENT, 110));
    // Corner ticks only on the two square corners; the chamfered ones carry
    // the cut itself as their accent.
    let tick = Stroke::new(1.5, ACCENT);
    p.line_segment([rect.left_top(), rect.left_top() + vec2(10.0, 0.0)], tick);
    p.line_segment([rect.left_top(), rect.left_top() + vec2(0.0, 10.0)], tick);
    p.line_segment(
        [rect.right_bottom(), rect.right_bottom() - vec2(10.0, 0.0)],
        tick,
    );
    p.line_segment(
        [rect.right_bottom(), rect.right_bottom() - vec2(0.0, 10.0)],
        tick,
    );

    let inner = Rect::from_min_max(
        pos2(l + 10.0, band_bottom + 8.0),
        pos2(r - 10.0, rect.bottom() - 10.0),
    );
    let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::default()));
    child.set_clip_rect(inner.expand(2.0).intersect(ui.clip_rect()));
    add(&mut child)
}

/// Small tracked-caps label inside a module (e.g. STAGED / UNSTAGED groups).
pub(crate) fn sub_label(ui: &Ui, rect: Rect, text: &str) {
    theme::tracked_text(
        ui.painter(),
        pos2(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        text,
        theme::display_font(11.5),
        DIM,
        1.5,
    );
}

/// Commit age as a coarse relative time ("3h ago"). Shared by the graph and
/// the commit-detail panel.
pub(crate) fn rel_time(commit_time: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let s = (now - commit_time).max(0);
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;
    if s < MIN {
        "now".to_owned()
    } else if s < HOUR {
        format!("{}m ago", s / MIN)
    } else if s < DAY {
        format!("{}h ago", s / HOUR)
    } else if s < MONTH {
        format!("{}d ago", s / DAY)
    } else if s < YEAR {
        format!("{}mo ago", s / MONTH)
    } else {
        format!("{}y ago", s / YEAR)
    }
}

/// Monospace glyph width -- the diff viewer's gutter math assumes one width
/// per font, which holds for its fixed-width font.
pub(crate) fn char_w(ui: &Ui, font: &FontId) -> f32 {
    ui.ctx().fonts_mut(|f| f.glyph_width(font, 'M'))
}

/// Rendered width of `s` in `font`, in points. egui lays text out glyph by
/// glyph with no kerning, so summing glyph advances is exactly the width it
/// will draw -- accurate for proportional fonts, not just monospace.
pub(crate) fn text_width(ui: &Ui, s: &str, font: &FontId) -> f32 {
    ui.ctx()
        .fonts_mut(|f| s.chars().map(|c| f.glyph_width(font, c)).sum())
}

/// Truncate keeping the *end* of the string (so filenames stay visible), so it
/// renders within `max_w` points; a leading "…" marks where it was cut.
pub(crate) fn truncate_left(ui: &Ui, s: &str, font: &FontId, max_w: f32) -> String {
    ui.ctx().fonts_mut(|f| {
        if s.chars().map(|c| f.glyph_width(font, c)).sum::<f32>() <= max_w {
            return s.to_owned();
        }
        let budget = (max_w - f.glyph_width(font, '…')).max(0.0);
        let mut used = 0.0;
        let mut keep = 0;
        // Walk from the end, keeping as many trailing chars as fit the budget.
        for c in s.chars().rev() {
            let w = f.glyph_width(font, c);
            if used + w > budget {
                break;
            }
            used += w;
            keep += 1;
        }
        let tail: String = s.chars().skip(s.chars().count() - keep).collect();
        format!("…{tail}")
    })
}

/// Truncate keeping the *start* of the string so it renders within `max_w`
/// points; a trailing "…" marks where it was cut.
pub(crate) fn truncate_right(ui: &Ui, s: &str, font: &FontId, max_w: f32) -> String {
    ui.ctx().fonts_mut(|f| {
        if s.chars().map(|c| f.glyph_width(font, c)).sum::<f32>() <= max_w {
            return s.to_owned();
        }
        let budget = (max_w - f.glyph_width(font, '…')).max(0.0);
        let mut used = 0.0;
        let mut head = String::new();
        for c in s.chars() {
            let w = f.glyph_width(font, c);
            if used + w > budget {
                break;
            }
            used += w;
            head.push(c);
        }
        format!("{head}…")
    })
}

//! Entrance animations for overlay surfaces -- modals and context menus.
//!
//! Nothing in the HUD pops into existence; it boots up. A single eased factor
//! `t` (0 -> 1) drives a backdrop fade, a plate that develops up out of the
//! void, a scanline that wipes down it, a glow that ignites, and target-lock
//! brackets that snap onto the corners -- the same target-lock vocabulary
//! `HudButton` plays on hover, so the whole app speaks one motion language.
//!
//! The factor is read off the surface's *real* open state every frame,
//! including while it animates shut, so egui keeps decaying it to 0 and the
//! next open replays from the start. Surfaces draw only while open: the close
//! is instant by design (immediate mode has no torn-down content to animate).

use eframe::egui::{
    Align2, Color32, Context, Frame, Id, InnerResponse, Margin, Modal, Painter, Pos2, Rect,
    Response, Sense, Stroke, Ui, emath::TSTransform, emath::easing, lerp, pos2, vec2,
};

use crate::theme::{self, BG0, BG1, BRIGHT};

/// Boot-up duration. Snappy enough to feel responsive, slow enough to read as
/// deliberate machinery rather than a flick.
const DUR: f32 = 0.17;

/// The eased open factor for an overlay, 0 (hidden) -> 1 (settled). Call it
/// every frame with the surface's real open state -- even while it's closing --
/// so the value decays cleanly and the next open replays from zero. `base`
/// must be stable per surface.
pub(crate) fn factor(ctx: &Context, base: Id, open: bool) -> f32 {
    ctx.animate_bool_with_time_and_easing(base.with("reveal"), open, DUR, easing::cubic_out)
}

/// Angular HUD modal shell: a plate that develops up out of the void behind an
/// accent frame, a fading backdrop, a tracked title, and a target-lock
/// entrance. `t` is the open factor from [`factor`]. Returns the body's value
/// and whether the modal asked to close (backdrop click or Escape).
pub(crate) fn hud_modal<R>(
    ui: &mut Ui,
    id: &str,
    title: &str,
    accent: Color32,
    width: f32,
    t: f32,
    body: impl FnOnce(&mut Ui) -> R,
) -> (R, bool) {
    // The plate lifts out of the void as the frame ignites to the accent.
    let frame = Frame::new()
        .fill(BG0.lerp_to_gamma(BG1, t))
        .stroke(Stroke::new(1.0, theme::alpha(accent, (255.0 * t) as u8)))
        .inner_margin(Margin::same(14));
    let modal = Modal::new(Id::new(id))
        .frame(frame)
        .backdrop_color(theme::alpha(BG0, (215.0 * t) as u8))
        .show(ui.ctx(), |ui| {
            ui.set_width(width);
            modal_header(ui, title, accent);
            let inner = body(ui);
            // The frame border sits one inner-margin (14px) outside the
            // content; reach out to it so the entrance dressing hugs the plate.
            entrance(ui.painter(), ui.min_rect().expand(13.0), accent, t);
            inner
        });
    let closed = modal.should_close();
    (modal.inner, closed)
}

/// The tracked title and fade rule atop every HUD modal.
fn modal_header(ui: &mut Ui, title: &str, accent: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
    theme::tracked_text(
        ui.painter(),
        pos2(r.left(), r.center().y),
        Align2::LEFT_CENTER,
        title,
        theme::display_font(13.0),
        accent,
        2.5,
    );
    let (rule, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
    theme::fade_rule(
        ui.painter(),
        pos2(rule.left(), rule.center().y),
        rule.width(),
        accent,
    );
    ui.add_space(6.0);
}

/// The shared boot-up dressing painted over a surface's outline: target-lock
/// brackets that converge as it settles, a glow bloom that ignites, and a
/// scanline that wipes down the plate mid-transition. All driven by `t`.
fn entrance(p: &Painter, rect: Rect, accent: Color32, t: f32) {
    if t <= 0.0 {
        return;
    }
    // Brackets start flung out and snap onto the corners, arms extending in.
    let reach = 4.0 + 22.0 * (1.0 - t);
    theme::corner_brackets(
        p,
        rect.expand(reach),
        13.0 * t,
        Stroke::new(1.5, theme::alpha(accent, (230.0 * t) as u8)),
    );
    // Bloom around the plate silhouette, intensifying as it locks in.
    theme::glow_passes(p, &corners(rect), accent, t);
    // Scanline wipe: a bright line travels top -> bottom, brightest mid-reveal.
    if t < 1.0 {
        let env = (t * std::f32::consts::PI).sin();
        let y = lerp(rect.top()..=rect.bottom(), t);
        let clip = p.with_clip_rect(rect.expand(2.0));
        for (w, a) in [(6.0, 26.0), (2.0, 120.0)] {
            clip.line_segment(
                [pos2(rect.left(), y), pos2(rect.right(), y)],
                Stroke::new(w, theme::alpha(BRIGHT, (a * env) as u8)),
            );
        }
    }
}

/// `rect`'s four corners as a closed polygon, for the glow passes.
fn corners(rect: Rect) -> [Pos2; 4] {
    [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ]
}

/// Wraps a context menu so it boots up like the rest of the HUD: the popup
/// scales and rises out of the cursor instead of snapping in. A context menu's
/// layer carries no backdrop, so transforming the whole layer is safe here --
/// unlike modals, where the scrim shares the layer. Items, dismissal, and
/// keyboard nav stay egui's job.
pub(crate) fn hud_menu(resp: &Response, add: impl FnOnce(&mut Ui)) {
    let inner = resp.context_menu(add);
    let t = factor(&resp.ctx, resp.id.with("hud_menu"), inner.is_some());
    if let Some(InnerResponse { response, .. }) = inner {
        if t < 1.0 {
            // Scale + rise about the top-left so the popup unfolds from the
            // click point rather than ballooning out of its own center.
            let anchor = response.rect.left_top().to_vec2();
            let tf = TSTransform::from_translation(anchor + vec2(0.0, (1.0 - t) * 9.0))
                * TSTransform::from_scaling(lerp(0.85..=1.0, t))
                * TSTransform::from_translation(-anchor);
            resp.ctx.transform_layer_shapes(response.layer_id, tf);
        }
    }
}

//! Shared UI chrome: top bar, status bar, empty state, and text helpers.

pub mod diff;
pub mod history;
pub mod sidebar;

use eframe::egui::{
    Align, Button, FontId, Frame, Layout, Margin, RichText, Spinner, Stroke, Ui, pos2,
};

use crate::app::RepoTab;
use crate::theme::{ACCENT, AMBER, BG1, BG2, BORDER, DIM, RED, TEXT};

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
        ui.label(RichText::new("▰ GITGUD").color(ACCENT).strong());
        ui.separator();

        for (i, tab) in tabs.iter().enumerate() {
            let is_active = i == active;
            let stroke = Stroke::new(1.0, if is_active { ACCENT } else { BORDER });
            Frame::new()
                .fill(if is_active { BG2 } else { BG1 })
                .stroke(stroke)
                .inner_margin(Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    let color = if is_active { TEXT } else { DIM };
                    let name = Button::new(RichText::new(&tab.name).color(color)).frame(false);
                    if ui.add(name).clicked() {
                        action = Some(TopBarAction::Select(i));
                    }
                    let close = Button::new(RichText::new("✕").small().color(DIM)).frame(false);
                    if ui.add(close).clicked() {
                        action = Some(TopBarAction::Close(i));
                    }
                });
        }

        if ui.button("+ OPEN").clicked() {
            action = Some(TopBarAction::Open);
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Both network actions need a loaded repo; disabled until then.
            let ready = tabs.get(active).is_some_and(|t| t.loaded);
            if ui.add_enabled(ready, Button::new("⟳ FETCH")).clicked() {
                action = Some(TopBarAction::Fetch);
            }
            if ui.add_enabled(ready, Button::new("⤒ PUSH")).clicked() {
                action = Some(TopBarAction::Push);
            }
            if tabs.iter().any(|t| t.in_flight > 0) {
                ui.label(RichText::new("BUSY").small().color(DIM));
                ui.add(Spinner::new().size(14.0).color(ACCENT));
            }
        });
    });
    action
}

pub fn status_bar(ui: &mut Ui, tab: Option<&RepoTab>, app_log: Option<&(bool, String)>) {
    let bar = ui.max_rect();
    ui.horizontal_centered(|ui| {
        if let Some(tab) = tab {
            let glyph = if tab.head_detached { "⌖" } else { "⎇" };
            ui.label(RichText::new(format!("{glyph} {}", tab.head)).color(AMBER));
            ui.label(RichText::new(tab.path.display().to_string()).color(DIM));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Per-tab log wins; the app log catches errors without a tab.
            if let Some((is_err, msg)) = tab.and_then(|t| t.log.as_ref()).or(app_log) {
                let color = if *is_err { RED } else { DIM };
                ui.label(RichText::new(msg).color(color));
            }
        });
    });

    if tab.is_some_and(|t| t.in_flight > 0) {
        // Animated scanline along the bar's top edge while jobs are in flight.
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
    ui.add_space(ui.available_height() * 0.32);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("▰▰ GITGUD").font(FontId::monospace(28.0)).color(ACCENT));
        ui.label(RichText::new("tactical git operations console").color(DIM));
        ui.add_space(16.0);
        open = ui.button("OPEN REPOSITORY").clicked();
        ui.add_space(8.0);
        ui.label(RichText::new("or drop a folder anywhere").small().color(DIM));
    });
    open
}

pub(crate) fn section_header(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).small().color(DIM));
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
    ui.ctx().fonts_mut(|f| s.chars().map(|c| f.glyph_width(font, c)).sum())
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

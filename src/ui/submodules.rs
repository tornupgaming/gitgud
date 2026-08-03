//! SUBMODULES panel: an overview of each submodule and its state, with
//! open-as-tab and init/update actions. Pointer changes (a recorded submodule
//! commit moving) surface separately as `ChangeKind::Submodule` rows in the
//! working tree; this panel is the registry view and the action surface.

use eframe::egui::{Align2, CornerRadius, Sense, Stroke, TextStyle, Ui, pos2, vec2};

use super::{HudButton, module, truncate_left, vscroll};
use crate::app::RepoTab;
use crate::git::Job;
use crate::theme::{AMBER, BG3, DIM, GREEN, PURPLE};

/// What a row asked for: open the submodule in its own tab, or run an
/// init/update (`None` path = all submodules).
enum SubAction {
    Open(String),
    Update(Option<String>),
}

/// The SUBMODULES module. Caller only invokes this when the repo actually has
/// submodules, so the panel never shows empty.
pub fn show(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let readout = format!("{:02}", tab.submodules.len());
    let action = module(ui, None, "SUBMODULES", &readout, |ui| {
        let mut action = None;
        if ui
            .add(
                HudButton::new("⟳ UPDATE ALL")
                    .small(true)
                    .min_size(vec2(ui.available_width(), 22.0)),
            )
            .clicked()
        {
            action = Some(SubAction::Update(None));
        }
        ui.add_space(6.0);
        vscroll("submodules").show(ui, |ui| {
            if let Some(a) = rows(ui, tab) {
                action = Some(a);
            }
        });
        action
    });
    match action {
        // Armed; the frame loop opens it after layout (needs `&mut self`).
        Some(SubAction::Open(path)) => tab.pending_open_submodule = Some(path),
        Some(SubAction::Update(path)) => jobs.push(Job::UpdateSubmodules {
            id: tab.id,
            path,
            init: true,
        }),
        None => {}
    }
}

/// Draws one row per submodule. Left-click opens it as a tab (the common move:
/// go fix the submodule, then come back and stage the pointer); the right-click
/// menu carries the same open plus a per-submodule update.
fn rows(ui: &mut Ui, tab: &RepoTab) -> Option<SubAction> {
    let font = TextStyle::Body.resolve(ui.style());
    let row_h = ui.text_style_height(&TextStyle::Body) + 6.0;

    let mut action = None;
    for s in &tab.submodules {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let p = ui.painter().with_clip_rect(rect);
        if resp.hovered() {
            p.rect_filled(rect, CornerRadius::ZERO, BG3);
        }
        let y = rect.center().y;

        // Colour encodes the headline state, in priority order: a pointer that
        // moved (staged or not) outranks mere dirtiness, which outranks clean.
        let color = if !s.initialized {
            DIM
        } else if s.new_commits || s.staged {
            PURPLE
        } else if s.dirty {
            AMBER
        } else {
            GREEN
        };
        // Filled node when checked out, hollow when only registered -- mirrors
        // the branch panel's local/remote marker idiom.
        if s.initialized {
            p.circle_filled(pos2(rect.left() + 9.0, y), 3.0, color);
        } else {
            p.circle_stroke(pos2(rect.left() + 9.0, y), 3.0, Stroke::new(1.0_f32, color));
        }

        // Right readout: the checked-out short id, or "uninit" prompting an
        // init. Painted first so the path can be truncated to the gap it leaves.
        let right_label = if !s.initialized {
            "uninit"
        } else if s.head_short.is_empty() {
            "—"
        } else {
            s.head_short.as_str()
        };
        let rt = p.text(
            pos2(rect.right() - 4.0, y),
            Align2::RIGHT_CENTER,
            right_label,
            font.clone(),
            DIM,
        );
        let text_w = (rt.left() - 6.0 - (rect.left() + 20.0)).max(0.0);
        let path = truncate_left(ui, &s.path, &font, text_w);
        p.text(
            pos2(rect.left() + 20.0, y),
            Align2::LEFT_CENTER,
            path,
            font.clone(),
            color,
        );

        if resp.clicked() {
            action = Some(SubAction::Open(s.path.clone()));
        }
        super::hud_menu(&resp, |ui| {
            if ui.button("Open as tab").clicked() {
                action = Some(SubAction::Open(s.path.clone()));
                ui.close();
            }
            let label = if s.initialized {
                "Update"
            } else {
                "Init + update"
            };
            if ui.button(label).clicked() {
                action = Some(SubAction::Update(Some(s.path.clone())));
                ui.close();
            }
        });
    }
    action
}

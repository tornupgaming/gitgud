//! Global application settings modal.

use eframe::egui::{Align, ComboBox, DragValue, Id, Layout, RichText, Ui};

use super::HudButton;
use crate::app::{StaleBranchSettings, StaleCutoffUnit};
use crate::theme::{ACCENT, DIM};

/// Edit a copied preference and return it only when APPLY is pressed. Keeping
/// a draft means closing or cancelling never mutates the live branch list.
pub fn show(ui: &mut Ui, draft: &mut Option<StaleBranchSettings>) -> Option<StaleBranchSettings> {
    let t = super::factor(ui.ctx(), Id::new("settings"), draft.is_some());
    let mut value = (*draft)?;

    let ((apply, cancel), closed) =
        super::hud_modal(ui, "settings", "SETTINGS", ACCENT, 340.0, t, |ui| {
            ui.label("STALE BRANCH CUTOFF");
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Branches whose latest commit is older than this period are marked stale.",
                )
                .small()
                .color(DIM),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add(DragValue::new(&mut value.amount).range(1..=999).speed(1.0));
                ComboBox::from_id_salt("stale_cutoff_unit")
                    .selected_text(match value.unit {
                        StaleCutoffUnit::Weeks => "weeks",
                        StaleCutoffUnit::Months => "months",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut value.unit, StaleCutoffUnit::Weeks, "weeks");
                        ui.selectable_value(&mut value.unit, StaleCutoffUnit::Months, "months");
                    });
            });
            ui.add_space(5.0);
            ui.label(
                RichText::new("One month equals 30 days.")
                    .small()
                    .color(DIM),
            );
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let apply = ui.add(HudButton::new("APPLY")).clicked();
                let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                (apply, cancel)
            })
            .inner
        });

    if apply {
        *draft = None;
        Some(value)
    } else {
        *draft = if cancel || closed { None } else { Some(value) };
        None
    }
}

//! The progress modal shown while a long-running network operation
//! (fetch / push / pull / submodule update) is in flight, so a multi-second
//! round trip to the remote reads as work underway rather than a frozen
//! window.
//!
//! Unlike the reconcile dialog this modal makes no decision and takes no
//! input: it can't be dismissed because dismissing it wouldn't stop the
//! subprocess already running on the worker's network thread. It simply
//! vanishes when the operation's reply lands and `net_ops` drains -- the same
//! instant-close the rest of the HUD's overlays use.

use eframe::egui::{Id, RichText, Spinner, Ui};

use crate::app::RepoTab;
use crate::theme::{ACCENT, DIM};

/// Renders the in-progress modal while `tab.net_ops` holds a running network
/// op. The title is the op label (PUSH / PULL / ...); the body is a spinner, a
/// one-line status, and the reassurance that the wait is the network, not a
/// hang. Takes no input and emits no job -- the worker's reply closes it.
pub fn show(ui: &mut Ui, tab: &RepoTab) {
    // Read the open state every frame so the factor decays cleanly on close.
    let t = super::factor(
        ui.ctx(),
        Id::new("network_progress"),
        !tab.net_ops.is_empty(),
    );
    // Newest op wins the title when several overlap; closing draws nothing.
    let Some(&label) = tab.net_ops.last() else {
        return;
    };
    let status = describe(label, &tab.head);

    super::hud_modal(ui, "network_progress", label, ACCENT, 320.0, t, |ui| {
        ui.horizontal(|ui| {
            ui.add(Spinner::new().size(20.0).color(ACCENT));
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(status).color(ACCENT));
                ui.label(
                    RichText::new("Talking to the remote — this can take a moment.")
                        .small()
                        .color(DIM),
                );
            });
        });
    });

    // The spinner is animated; keep the frame ticking while the modal is up.
    ui.ctx().request_repaint();
}

/// One-line status for a network op label, naming the branch where it helps.
fn describe(label: &str, head: &str) -> String {
    match label {
        "FETCH" => "Fetching refs from every remote".to_string(),
        "PUSH" => format!("Pushing \"{head}\" to origin"),
        "PULL" => format!("Pulling \"{head}\" from its upstream"),
        "SUBMODULES" => "Updating submodules".to_string(),
        _ => "Working".to_string(),
    }
}

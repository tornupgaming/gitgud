//! The progress modal shown while a long-running network operation
//! (fetch / push / pull / submodule update) is in flight, so a multi-second
//! round trip to the remote reads as work underway rather than a frozen
//! window.
//!
//! It shows two things the bare spinner couldn't: a **checklist** of the steps
//! the op runs on our side (probe the remote, run the pre-push hook, push), each
//! ticking from a spinner to a check or cross as it finishes; and a **console
//! tail** of the last lines the git subprocess actually printed, so transfer
//! progress and hook output (lint/format/test) are visible live -- genuine
//! evidence the work is moving, not hung. Both are fed by the worker's
//! `Update::Progress` stream into `tab.net_ops`.
//!
//! Unlike the reconcile dialog this modal makes no decision and takes no
//! input: it can't be dismissed because dismissing it wouldn't stop the
//! subprocess already running on the worker's network thread. It simply
//! vanishes when the operation's reply lands and `net_ops` drains -- the same
//! instant-close the rest of the HUD's overlays use.

use eframe::egui::{Align2, FontId, Id, RichText, Sense, Spinner, Ui, vec2};

use crate::app::{OpProgress, RepoTab};
use crate::git::StepState;
use crate::theme::{self, ACCENT, BRIGHT, DIM, GREEN, RED};

/// How many trailing console lines the tail shows. Two is enough to read a
/// "remote: ..." line and the transfer-progress line under it without the modal
/// growing tall or jittering.
const TAIL_LINES: usize = 2;

/// Renders the in-progress modal while `tab.net_ops` holds a running network
/// op. The title is the op label (PUSH / PULL / ...); the body is the step
/// checklist and a console tail. Takes no input and emits no job -- the
/// worker's reply closes it.
pub fn show(ui: &mut Ui, tab: &RepoTab) {
    // Read the open state every frame so the factor decays cleanly on close.
    let t = super::factor(
        ui.ctx(),
        Id::new("network_progress"),
        !tab.net_ops.is_empty(),
    );
    // Newest op wins the modal when several overlap; closing draws nothing.
    let Some(op) = tab.net_ops.last() else {
        return;
    };
    let subtitle = describe(op.op, &tab.head);

    super::hud_modal(ui, "network_progress", op.op, ACCENT, 360.0, t, |ui| {
        ui.label(RichText::new(subtitle).color(ACCENT));
        ui.add_space(10.0);
        checklist(ui, op);
        ui.add_space(10.0);
        console_tail(ui, op);
    });

    // The spinner is animated; keep the frame ticking while the modal is up.
    ui.ctx().request_repaint();
}

/// The step rows: a leading status glyph (spinner / check / cross) then the
/// label, the running step lit and finished ones dimmed. Before the first step
/// arrives, a lone "Starting…" spinner stands in so the modal is never empty.
fn checklist(ui: &mut Ui, op: &OpProgress) {
    if op.steps.is_empty() {
        ui.horizontal(|ui| {
            ui.add(Spinner::new().size(15.0).color(ACCENT));
            ui.add_space(8.0);
            ui.label(RichText::new("Starting…").color(DIM));
        });
        return;
    }
    for step in &op.steps {
        ui.horizontal(|ui| {
            step_glyph(ui, step.state);
            ui.add_space(8.0);
            let color = match step.state {
                StepState::Running => BRIGHT,
                StepState::Done => DIM,
                StepState::Failed => RED,
            };
            ui.label(RichText::new(&step.label).color(color));
        });
        ui.add_space(2.0);
    }
}

/// Draws the leading status indicator for a step: an animated spinner while
/// running, a green check when done, a red cross when failed.
fn step_glyph(ui: &mut Ui, state: StepState) {
    match state {
        StepState::Running => {
            ui.add(Spinner::new().size(15.0).color(ACCENT));
        }
        StepState::Done | StepState::Failed => {
            let (glyph, color) = if state == StepState::Done {
                ("✓", GREEN)
            } else {
                ("✗", RED)
            };
            let (rect, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
            theme::tracked_text(
                ui.painter(),
                rect.center(),
                Align2::CENTER_CENTER,
                glyph,
                FontId::proportional(14.0),
                color,
                0.0,
            );
        }
    }
}

/// The console tail: the last [`TAIL_LINES`] lines the subprocess printed, in
/// dim monospace, each clipped to one line so a long remote message can't widen
/// the modal. A reassurance line stands in until the first output arrives.
fn console_tail(ui: &mut Ui, op: &OpProgress) {
    let (rule, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
    theme::fade_rule(
        ui.painter(),
        rule.left_center(),
        rule.width(),
        ACCENT,
    );
    ui.add_space(4.0);

    if op.console.is_empty() {
        ui.label(
            RichText::new("Talking to the remote — this can take a moment.")
                .small()
                .color(DIM),
        );
        return;
    }

    let font = FontId::monospace(11.0);
    let avail = ui.available_width();
    let start = op.console.len().saturating_sub(TAIL_LINES);
    for line in &op.console[start..] {
        let shown = super::truncate_right(ui, line, &font, avail);
        ui.label(RichText::new(shown).monospace().size(11.0).color(DIM));
    }
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

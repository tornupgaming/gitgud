# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**gitgud** — a Windows git GUI client (~4k lines of Rust) built on eframe/egui with a sci-fi HUD aesthetic. Binary name `gitgud`, window title "GITGUD". Rust edition 2024.

## Commands

- `cargo check` — fastest verification; run after edits.
- `cargo run` — build and launch the app.
- `cargo test` — run unit tests (they live in `#[cfg(test)]` modules in `src/git/ops.rs` and `src/ui/highlight.rs`).
- `cargo test <name>` — run a single test.
- `cargo clippy` / `cargo fmt` — lint and format.

The release profile is aggressively size-optimized (`opt-level = "s"`, LTO, strip, `panic = "abort"`) — binary size is a tracked feature; don't add dependencies casually, and keep `default-features = false` trimming intact.

## Architecture

Two threads, message-passing, no locks:

- **UI thread** (`src/app.rs`) never touches `git2::Repository`. It sends `Job` enum values to the worker and drains `Update` replies once per frame via `GitWorker::poll()`.
- **Worker thread** (`src/git/worker.rs`) owns all `Repository` handles (they are `Send` but not `Sync`) in a `HashMap<RepoId, _>` and executes jobs sequentially. Network jobs (fetch/push/pull) get their own throwaway thread so they don't block local jobs; these shell out to the **git CLI** (not libgit2) so the user's credential helpers work.
- **Contract** (`src/git/mod.rs`): `Job`, `Update`, and all snapshot types. Everything crossing the channel is plain owned data (`String`, `Vec`). Every job except `Close` produces exactly one `Update`.
- **Operations** (`src/git/ops.rs`): pure functions `Repository -> plain data`, called only from the worker. Errors become `String` via the local `g()` helper.

Key invariants in `app.rs`:

- All jobs go through `GitGudApp::send()`, which increments the per-tab `in_flight` counter — the busy indicator depends on this. Don't call `worker.send()` directly.
- During panel layout the active `RepoTab` is borrowed mutably, so UI clicks push into a local `Vec<Job>` that is sent after layout ends.
- Diff replies echo their `DiffKey`; the UI drops replies that no longer match the current selection. After a successful mutating action, the app re-sends `Refresh` (and `LoadHistory` for commit/checkout/fetch/push/pull).
- Destructive actions (checkout, discard) are arm-then-confirm: `pending_checkout` / `pending_discard` hold the armed state and the job only fires on confirm.

UI (`src/ui/`): immediate-mode egui. Every panel is a "module" — the chamfered HUD plate drawn by `module()` in `ui/mod.rs`. All colors, fonts (Rajdhani, bundled in `assets/fonts/`), and paint primitives (scanlines, glow, chamfer, gradient fills) come from `src/theme.rs`; never hardcode colors. `shell.rs` = top/status bars, `sidebar.rs` = branches + stage/unstage, `history.rs` = commit log (paged, 250/page), `diff.rs` = diff viewer (flattens `DiffData` into pre-highlighted rows once, then virtualizes with `show_rows`), `highlight.rs` = syntect highlighting with a custom HUD theme and a bundled Odin grammar.

State persistence: open-repo list at `%APPDATA%\gitgud\repos.txt`. No other config.

Keyboard shortcuts: F5 refresh, Ctrl+Enter commit, Ctrl+P push; refresh also fires on window refocus.

//! Syntax highlighting for the diff view. One `LineHighlighter` per file
//! carries syntect's parse state across that file's lines, so multi-line
//! constructs (block comments, raw strings) colour correctly. Output is plain
//! byte-range spans into the line text -- the renderer stays a flat painter.

use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;

use eframe::egui::Color32;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, StyleModifier, Theme, ThemeItem, ThemeSettings};
use syntect::parsing::{SyntaxDefinition, SyntaxSet};

use crate::theme::{ACCENT, AMBER, BRIGHT, DIM, PURPLE, TEXT};

pub type Spans = Vec<(Color32, Range<usize>)>;

struct Assets {
    set: SyntaxSet,
    theme: Theme,
}

fn assets() -> &'static Assets {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    ASSETS.get_or_init(|| {
        let mut builder = SyntaxSet::load_defaults_newlines().into_builder();
        // Odin isn't in the Sublime default packages; ship our own grammar.
        if let Ok(odin) =
            SyntaxDefinition::load_from_str(include_str!("odin.sublime-syntax"), true, None)
        {
            builder.add(odin);
        }
        Assets {
            set: builder.build(),
            theme: hud_theme(),
        }
    })
}

/// Map token classes onto the HUD palette instead of importing a stock
/// editor theme, so code reads as part of the same instrument panel.
fn hud_theme() -> Theme {
    let item = |selector: &str, color: Color32| ThemeItem {
        scope: selector.parse().expect("static scope selector"),
        style: StyleModifier {
            foreground: Some(sc(color)),
            background: None,
            font_style: None,
        },
    };
    Theme {
        settings: ThemeSettings {
            foreground: Some(sc(TEXT)),
            ..Default::default()
        },
        scopes: vec![
            item("comment", DIM),
            item("string, constant.character", AMBER),
            item(
                "constant.numeric, constant.language, constant.other",
                PURPLE,
            ),
            item("keyword, storage", ACCENT),
            item(
                "entity.name, support.function, support.type, support.class, support.macro, \
                 variable.function",
                BRIGHT,
            ),
        ],
        ..Default::default()
    }
}

fn sc(c: Color32) -> Color {
    Color {
        r: c.r(),
        g: c.g(),
        b: c.b(),
        a: 255,
    }
}

pub struct LineHighlighter(Option<HighlightLines<'static>>);

impl LineHighlighter {
    pub fn for_path(path: &str) -> Self {
        let a = assets();
        let syntax = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .and_then(|ext| a.set.find_syntax_by_extension(&ext.to_ascii_lowercase()));
        Self(syntax.map(|s| HighlightLines::new(s, &a.theme)))
    }

    /// `None` when the file's language is unknown (caller falls back to a
    /// single-colour line).
    pub fn spans(&mut self, text: &str) -> Option<Spans> {
        // Oniguruma over a multi-megabyte line (minified JS, embedded blobs)
        // can stall the UI thread for seconds; such lines render uncoloured.
        const LINE_CAP: usize = 4096;
        if text.len() > LINE_CAP {
            return None;
        }
        let hl = self.0.as_mut()?;
        // The default syntaxes are compiled expecting the newline the diff
        // collector trimmed off; restore it, then clamp spans back.
        let buf = format!("{text}\n");
        let regions = hl.highlight_line(&buf, &assets().set).ok()?;
        let mut spans = Vec::with_capacity(regions.len());
        let mut start = 0;
        for (style, s) in regions {
            let end = (start + s.len()).min(text.len());
            if end > start {
                let c = style.foreground;
                spans.push((Color32::from_rgb(c.r, c.g, c.b), start..end));
            }
            start += s.len();
        }
        Some(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_contender_extensions_resolve() {
        for p in ["a.rs", "a.js", "a.cs", "a.odin"] {
            assert!(
                LineHighlighter::for_path(p).0.is_some(),
                "no syntax for {p}"
            );
        }
    }

    #[test]
    fn odin_line_gets_multiple_token_colours() {
        let mut hl = LineHighlighter::for_path("demo.odin");
        let spans = hl
            .spans("main :: proc() { fmt.println(\"hi\", 42) } // done")
            .unwrap();
        let distinct: std::collections::HashSet<_> = spans.iter().map(|(c, _)| *c).collect();
        assert!(distinct.len() >= 3, "spans: {spans:?}");
    }

    #[test]
    fn spans_cover_line_without_trailing_newline() {
        let mut hl = LineHighlighter::for_path("a.rs");
        let line = "let x = 1;";
        let spans = hl.spans(line).unwrap();
        assert_eq!(spans.last().unwrap().1.end, line.len());
    }
}

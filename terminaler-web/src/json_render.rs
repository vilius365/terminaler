use serde::Serialize;
use terminaler_cell::color::ColorAttribute;
use terminaler_cell::{CellAttributes, Intensity, Underline};
use terminaler_color_types::SrgbaTuple;
use terminaler_surface::Line;

/// A styled text span for JSON terminal rendering.
/// Consecutive cells with identical attributes are grouped into a single span.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Span {
    /// Text content.
    pub t: String,
    /// Foreground color: "#rrggbb" or "p{idx}" for palette index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    /// Background color.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    /// Bold.
    #[serde(skip_serializing_if = "is_false")]
    pub b: bool,
    /// Dim/half-brightness.
    #[serde(skip_serializing_if = "is_false")]
    pub d: bool,
    /// Italic.
    #[serde(skip_serializing_if = "is_false")]
    pub i: bool,
    /// Underline: 0=none, 1=single, 2=double, 3=curly, 4=dotted, 5=dashed.
    #[serde(skip_serializing_if = "is_zero")]
    pub u: u8,
    /// Strikethrough.
    #[serde(skip_serializing_if = "is_false")]
    pub s: bool,
    /// Reverse video.
    #[serde(skip_serializing_if = "is_false")]
    pub r: bool,
    /// Overline.
    #[serde(skip_serializing_if = "is_false")]
    pub o: bool,
}

/// Cursor position and shape information.
#[derive(Serialize, Debug, Clone)]
pub struct CursorInfo {
    pub x: usize,
    pub y: usize,
    pub shape: String,
    pub visible: bool,
}

fn is_false(v: &bool) -> bool {
    !v
}

fn is_zero(v: &u8) -> bool {
    *v == 0
}

/// Convert a slice of Lines to span arrays (one per line).
/// Groups consecutive cells with identical attributes into single spans.
pub fn lines_to_spans(lines: &[Line]) -> Vec<Vec<Span>> {
    lines.iter().map(line_to_spans).collect()
}

/// Convert a single Line to a vector of Spans.
fn line_to_spans(line: &Line) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut current_text = String::new();
    let mut current_attrs = CellAttributes::default();
    let mut first_cell = true;

    for cell in line.visible_cells() {
        let attrs = cell.attrs();
        let text = cell.str();
        let ch = if text.is_empty() || text == "\0" {
            " "
        } else {
            text
        };

        if first_cell {
            current_attrs = attrs.clone();
            current_text.push_str(ch);
            first_cell = false;
        } else if attrs_equal(&current_attrs, attrs) {
            current_text.push_str(ch);
        } else {
            // Flush current span
            spans.push(make_span(&current_text, &current_attrs));
            current_text.clear();
            current_text.push_str(ch);
            current_attrs = attrs.clone();
        }
    }

    // Flush remaining text
    if !current_text.is_empty() {
        spans.push(make_span(&current_text, &current_attrs));
    }

    spans
}

/// Build a Span from accumulated text and attributes.
fn make_span(text: &str, attrs: &CellAttributes) -> Span {
    let underline_val = match attrs.underline() {
        Underline::None => 0,
        Underline::Single => 1,
        Underline::Double => 2,
        Underline::Curly => 3,
        Underline::Dotted => 4,
        Underline::Dashed => 5,
    };

    Span {
        t: text.to_string(),
        fg: color_to_string(attrs.foreground()),
        bg: color_to_string(attrs.background()),
        b: attrs.intensity() == Intensity::Bold,
        d: attrs.intensity() == Intensity::Half,
        i: attrs.italic(),
        u: underline_val,
        s: attrs.strikethrough(),
        r: attrs.reverse(),
        o: attrs.overline(),
    }
}

/// Convert a ColorAttribute to a CSS-friendly string.
/// Returns None for Default (omitted from JSON).
fn color_to_string(color: ColorAttribute) -> Option<String> {
    match color {
        ColorAttribute::Default => None,
        ColorAttribute::PaletteIndex(idx) => Some(format!("p{}", idx)),
        ColorAttribute::TrueColorWithPaletteFallback(rgba, _)
        | ColorAttribute::TrueColorWithDefaultFallback(rgba) => Some(srgba_to_hex(rgba)),
    }
}

/// Convert SrgbaTuple to "#rrggbb" hex string.
fn srgba_to_hex(c: SrgbaTuple) -> String {
    let r = (c.0 * 255.0).clamp(0.0, 255.0) as u8;
    let g = (c.1 * 255.0).clamp(0.0, 255.0) as u8;
    let b = (c.2 * 255.0).clamp(0.0, 255.0) as u8;
    format!("#{:02x}{:02x}{:02x}", r, g, b)
}

/// Compare two CellAttributes for equality (same logic as ansi_render).
fn attrs_equal(a: &CellAttributes, b: &CellAttributes) -> bool {
    a.intensity() == b.intensity()
        && a.underline() == b.underline()
        && a.blink() == b.blink()
        && a.italic() == b.italic()
        && a.reverse() == b.reverse()
        && a.strikethrough() == b.strikethrough()
        && a.invisible() == b.invisible()
        && a.overline() == b.overline()
        && a.foreground() == b.foreground()
        && a.background() == b.background()
}

/// Convert a CursorShape enum to a string for JSON.
pub fn cursor_shape_to_string(shape: termwiz::surface::CursorShape) -> &'static str {
    use termwiz::surface::CursorShape;
    match shape {
        CursorShape::Default | CursorShape::BlinkingBlock | CursorShape::SteadyBlock => "block",
        CursorShape::BlinkingBar | CursorShape::SteadyBar => "bar",
        CursorShape::BlinkingUnderline | CursorShape::SteadyUnderline => "underline",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_lines() {
        let lines: Vec<Line> = vec![];
        let result = lines_to_spans(&lines);
        assert!(result.is_empty());
    }

    #[test]
    fn test_srgba_to_hex() {
        assert_eq!(srgba_to_hex(SrgbaTuple(1.0, 0.0, 0.0, 1.0)), "#ff0000");
        assert_eq!(srgba_to_hex(SrgbaTuple(0.0, 1.0, 0.0, 1.0)), "#00ff00");
        assert_eq!(srgba_to_hex(SrgbaTuple(0.0, 0.0, 1.0, 1.0)), "#0000ff");
        assert_eq!(srgba_to_hex(SrgbaTuple(0.0, 0.0, 0.0, 1.0)), "#000000");
        assert_eq!(srgba_to_hex(SrgbaTuple(1.0, 1.0, 1.0, 1.0)), "#ffffff");
    }

    #[test]
    fn test_color_to_string() {
        assert_eq!(color_to_string(ColorAttribute::Default), None);
        assert_eq!(
            color_to_string(ColorAttribute::PaletteIndex(1)),
            Some("p1".to_string())
        );
        assert_eq!(
            color_to_string(ColorAttribute::TrueColorWithDefaultFallback(SrgbaTuple(
                1.0, 0.0, 0.0, 1.0
            ))),
            Some("#ff0000".to_string())
        );
    }

    #[test]
    fn test_is_false_is_zero() {
        assert!(is_false(&false));
        assert!(!is_false(&true));
        assert!(is_zero(&0));
        assert!(!is_zero(&1));
    }

    #[test]
    fn test_span_serialization_minimal() {
        let span = Span {
            t: "hello".to_string(),
            fg: None,
            bg: None,
            b: false,
            d: false,
            i: false,
            u: 0,
            s: false,
            r: false,
            o: false,
        };
        let json = serde_json::to_string(&span).unwrap();
        // Only "t" should be present — all others skipped
        assert_eq!(json, r#"{"t":"hello"}"#);
    }

    #[test]
    fn test_span_serialization_with_attrs() {
        let span = Span {
            t: "bold".to_string(),
            fg: Some("#ff0000".to_string()),
            bg: None,
            b: true,
            d: false,
            i: false,
            u: 1,
            s: false,
            r: false,
            o: false,
        };
        let json = serde_json::to_string(&span).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["t"], "bold");
        assert_eq!(parsed["fg"], "#ff0000");
        assert_eq!(parsed["b"], true);
        assert_eq!(parsed["u"], 1);
        // bg, d, i, s, r, o should be absent
        assert!(parsed.get("bg").is_none());
        assert!(parsed.get("d").is_none());
    }

    #[test]
    fn test_cursor_shape_to_string() {
        use termwiz::surface::CursorShape;
        assert_eq!(cursor_shape_to_string(CursorShape::Default), "block");
        assert_eq!(cursor_shape_to_string(CursorShape::BlinkingBar), "bar");
        assert_eq!(
            cursor_shape_to_string(CursorShape::SteadyUnderline),
            "underline"
        );
    }
}

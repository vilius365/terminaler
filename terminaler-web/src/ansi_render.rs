use std::fmt::Write;
use terminaler_cell::color::ColorAttribute;
use terminaler_cell::{Blink, CellAttributes, Intensity, Underline};
use terminaler_color_types::SrgbaTuple;
use terminaler_surface::Line;

/// Render a set of lines as ANSI escape sequences suitable for xterm.js.
/// Each line is preceded by cursor positioning and line-clear sequences.
/// `first_row` is the 1-based row number for the first line in the slice.
pub fn lines_to_ansi(lines: &[Line], first_row: usize) -> String {
    let mut out = String::with_capacity(lines.len() * 128);
    let mut prev_attrs = CellAttributes::default();

    for (i, line) in lines.iter().enumerate() {
        let row = first_row + i;
        // Move cursor to row, column 1 and erase line
        write!(out, "\x1b[{row};1H\x1b[2K").unwrap();

        let mut col = 0;
        for cell in line.visible_cells() {
            let attrs = cell.attrs();
            emit_sgr_diff(&mut out, &prev_attrs, attrs);
            prev_attrs = attrs.clone();

            let text = cell.str();
            if text.is_empty() || text == "\0" {
                out.push(' ');
            } else {
                out.push_str(text);
            }
            col += 1;
        }
        let _ = col; // suppress unused warning
    }

    // Reset attributes at the end
    out.push_str("\x1b[0m");
    out
}

/// Render a full screen refresh (all visible lines) for xterm.js.
pub fn full_screen_ansi(lines: &[Line], cols: usize, rows: usize) -> String {
    let mut out = String::with_capacity(lines.len() * cols * 2);

    // Clear screen and move to top-left
    out.push_str("\x1b[2J\x1b[H");

    let mut prev_attrs = CellAttributes::default();

    for (i, line) in lines.iter().enumerate() {
        if i >= rows {
            break;
        }
        if i > 0 {
            out.push_str("\r\n");
        }

        for cell in line.visible_cells() {
            let attrs = cell.attrs();
            emit_sgr_diff(&mut out, &prev_attrs, attrs);
            prev_attrs = attrs.clone();

            let text = cell.str();
            if text.is_empty() || text == "\0" {
                out.push(' ');
            } else {
                out.push_str(text);
            }
        }
    }

    // Reset at end
    out.push_str("\x1b[0m");
    out
}

/// Render scrollback history + viewport for xterm.js.
///
/// Scrollback lines are written as flowing text so xterm.js accumulates them
/// in its scrollback buffer. Then the viewport is cleared and rewritten with
/// cursor positioning, so delta updates (which also use cursor positioning)
/// overwrite cleanly without creating duplicates.
///
/// `cursor_row` and `cursor_col` are 1-based viewport-relative positions.
pub fn full_refresh_with_scrollback(
    scrollback_lines: &[Line],
    viewport_lines: &[Line],
    cols: usize,
    viewport_rows: usize,
    cursor_row: usize,
    cursor_col: usize,
) -> String {
    let total_lines = scrollback_lines.len() + viewport_rows;
    let mut out = String::with_capacity(total_lines * cols * 2);

    // Full terminal reset (clears screen + scrollback buffer in xterm.js)
    out.push_str("\x1bc");

    // Phase 1: Write scrollback lines as flowing text.
    // These accumulate in xterm.js's scrollback buffer.
    let mut prev_attrs = CellAttributes::default();
    for (i, line) in scrollback_lines.iter().enumerate() {
        if i > 0 {
            out.push_str("\r\n");
        }
        for cell in line.visible_cells() {
            let attrs = cell.attrs();
            emit_sgr_diff(&mut out, &prev_attrs, attrs);
            prev_attrs = attrs.clone();
            let text = cell.str();
            if text.is_empty() || text == "\0" {
                out.push(' ');
            } else {
                out.push_str(text);
            }
        }
    }

    // Push all scrollback content above the viewport by emitting
    // viewport_rows worth of newlines. This ensures every scrollback line
    // is in xterm.js's scrollback buffer, not the visible viewport.
    if !scrollback_lines.is_empty() {
        for _ in 0..viewport_rows {
            out.push_str("\r\n");
        }
    }

    // Reset attributes before viewport phase
    out.push_str("\x1b[0m");

    // Phase 2: Write viewport lines with cursor positioning.
    // This overwrites the blank lines pushed in above and matches
    // the format used by delta updates (lines_to_ansi), so subsequent
    // deltas overwrite these rows cleanly.
    prev_attrs = CellAttributes::default();
    for (i, line) in viewport_lines.iter().enumerate() {
        if i >= viewport_rows {
            break;
        }
        let row = i + 1; // 1-based
        write!(out, "\x1b[{row};1H\x1b[2K").unwrap();
        for cell in line.visible_cells() {
            let attrs = cell.attrs();
            emit_sgr_diff(&mut out, &prev_attrs, attrs);
            prev_attrs = attrs.clone();
            let text = cell.str();
            if text.is_empty() || text == "\0" {
                out.push(' ');
            } else {
                out.push_str(text);
            }
        }
    }

    // Reset attributes and position cursor
    out.push_str("\x1b[0m");
    write!(out, "\x1b[{};{}H", cursor_row, cursor_col).unwrap();

    out
}

/// Emit SGR (Select Graphic Rendition) escape sequences for attribute changes.
fn emit_sgr_diff(out: &mut String, prev: &CellAttributes, next: &CellAttributes) {
    // Quick path: if attributes are identical, skip
    if attrs_equal(prev, next) {
        return;
    }

    // Simple approach: emit a reset + full re-specification if anything changed
    // This is slightly verbose but correct and avoids complex diff logic
    let mut codes: Vec<u32> = Vec::new();

    // Always start with reset
    codes.push(0);

    // Intensity
    match next.intensity() {
        Intensity::Normal => {}
        Intensity::Bold => codes.push(1),
        Intensity::Half => codes.push(2),
    }

    // Italic
    if next.italic() {
        codes.push(3);
    }

    // Underline
    match next.underline() {
        Underline::None => {}
        Underline::Single => codes.push(4),
        Underline::Double => codes.push(21),
        Underline::Curly => {
            // CSI 4:3 m for curly underline
            let sgr = format_codes(&codes);
            if !sgr.is_empty() {
                write!(out, "\x1b[{sgr}m").unwrap();
            }
            out.push_str("\x1b[4:3m");
            codes.clear();
        }
        Underline::Dotted => {
            let sgr = format_codes(&codes);
            if !sgr.is_empty() {
                write!(out, "\x1b[{sgr}m").unwrap();
            }
            out.push_str("\x1b[4:4m");
            codes.clear();
        }
        Underline::Dashed => {
            let sgr = format_codes(&codes);
            if !sgr.is_empty() {
                write!(out, "\x1b[{sgr}m").unwrap();
            }
            out.push_str("\x1b[4:5m");
            codes.clear();
        }
    }

    // Blink
    match next.blink() {
        Blink::None => {}
        Blink::Slow => codes.push(5),
        Blink::Rapid => codes.push(6),
    }

    // Reverse
    if next.reverse() {
        codes.push(7);
    }

    // Invisible
    if next.invisible() {
        codes.push(8);
    }

    // Strikethrough
    if next.strikethrough() {
        codes.push(9);
    }

    // Overline
    if next.overline() {
        codes.push(53);
    }

    // Emit accumulated codes
    if !codes.is_empty() {
        let sgr = format_codes(&codes);
        write!(out, "\x1b[{sgr}m").unwrap();
    }

    // Foreground color
    emit_color(out, next.foreground(), true);

    // Background color
    emit_color(out, next.background(), false);
}

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

fn format_codes(codes: &[u32]) -> String {
    codes
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(";")
}

fn emit_color(out: &mut String, color: ColorAttribute, is_fg: bool) {
    match color {
        ColorAttribute::Default => {}
        ColorAttribute::PaletteIndex(idx) => {
            if is_fg {
                write!(out, "\x1b[38;5;{idx}m").unwrap();
            } else {
                write!(out, "\x1b[48;5;{idx}m").unwrap();
            }
        }
        ColorAttribute::TrueColorWithPaletteFallback(rgba, _)
        | ColorAttribute::TrueColorWithDefaultFallback(rgba) => {
            let (r, g, b) = srgba_to_rgb(rgba);
            if is_fg {
                write!(out, "\x1b[38;2;{r};{g};{b}m").unwrap();
            } else {
                write!(out, "\x1b[48;2;{r};{g};{b}m").unwrap();
            }
        }
    }
}

fn srgba_to_rgb(c: SrgbaTuple) -> (u8, u8, u8) {
    (
        (c.0 * 255.0).clamp(0.0, 255.0) as u8,
        (c.1 * 255.0).clamp(0.0, 255.0) as u8,
        (c.2 * 255.0).clamp(0.0, 255.0) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_lines() {
        let lines: Vec<Line> = vec![];
        let result = lines_to_ansi(&lines, 1);
        assert_eq!(result, "\x1b[0m");
    }

    #[test]
    fn test_full_screen_ansi_empty() {
        let lines: Vec<Line> = vec![];
        let result = full_screen_ansi(&lines, 80, 24);
        assert_eq!(result, "\x1b[2J\x1b[H\x1b[0m");
    }
}

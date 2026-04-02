use crate::color::{Palette, RgbaColor};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeName {
    Dark,
    Light,
    Custom,
}

impl Default for ThemeName {
    fn default() -> Self {
        ThemeName::Dark
    }
}

impl std::fmt::Display for ThemeName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThemeName::Dark => write!(f, "dark"),
            ThemeName::Light => write!(f, "light"),
            ThemeName::Custom => write!(f, "custom"),
        }
    }
}

impl std::str::FromStr for ThemeName {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_lowercase().as_str() {
            "dark" => Ok(ThemeName::Dark),
            "light" => Ok(ThemeName::Light),
            "custom" => Ok(ThemeName::Custom),
            other => anyhow::bail!("unknown theme name: {}", other),
        }
    }
}

fn rgba(r: u8, g: u8, b: u8) -> RgbaColor {
    (r, g, b).into()
}

/// Neutral dark theme with pure grays and GitHub-style accents.
pub fn dark_theme() -> Palette {
    Palette {
        foreground: Some(rgba(0xe0, 0xe0, 0xe0)),
        background: Some(rgba(0x12, 0x12, 0x12)),
        cursor_fg: Some(rgba(0x12, 0x12, 0x12)),
        cursor_bg: Some(rgba(0xe0, 0xe0, 0xe0)),
        cursor_border: Some(rgba(0xe0, 0xe0, 0xe0)),
        selection_fg: Some(rgba(0xe0, 0xe0, 0xe0)),
        selection_bg: Some(rgba(0x3a, 0x3a, 0x3a)),
        ansi: Some([
            rgba(0x2e, 0x2e, 0x2e), // black
            rgba(0xf8, 0x51, 0x49), // red
            rgba(0x3f, 0xb9, 0x50), // green
            rgba(0xd2, 0x99, 0x22), // yellow
            rgba(0x4d, 0x9e, 0xff), // blue
            rgba(0xbc, 0x8c, 0xff), // magenta
            rgba(0x56, 0xd4, 0xdd), // cyan
            rgba(0x99, 0x99, 0x99), // white
        ]),
        brights: Some([
            rgba(0x66, 0x66, 0x66), // bright black
            rgba(0xff, 0x7b, 0x72), // bright red
            rgba(0x56, 0xd3, 0x64), // bright green
            rgba(0xe3, 0xb3, 0x41), // bright yellow
            rgba(0x79, 0xc0, 0xff), // bright blue
            rgba(0xd2, 0xa8, 0xff), // bright magenta
            rgba(0x76, 0xe4, 0xf7), // bright cyan
            rgba(0xe0, 0xe0, 0xe0), // bright white
        ]),
        indexed: HashMap::new(),
        scrollbar_thumb: Some(rgba(0x3a, 0x3a, 0x3a)),
        split: Some(rgba(0x3a, 0x3a, 0x3a)),
        tab_bar: None,
        visual_bell: None,
        compose_cursor: None,
        copy_mode_active_highlight_fg: None,
        copy_mode_active_highlight_bg: None,
        copy_mode_inactive_highlight_fg: None,
        copy_mode_inactive_highlight_bg: None,
        quick_select_label_fg: None,
        quick_select_label_bg: None,
        quick_select_match_fg: None,
        quick_select_match_bg: None,
        input_selector_label_fg: None,
        input_selector_label_bg: None,
        launcher_label_fg: None,
        launcher_label_bg: None,
    }
}

/// Light theme based on Catppuccin Latte palette.
pub fn light_theme() -> Palette {
    Palette {
        foreground: Some(rgba(0x4c, 0x4f, 0x69)),
        background: Some(rgba(0xef, 0xf1, 0xf5)),
        cursor_fg: Some(rgba(0xef, 0xf1, 0xf5)),
        cursor_bg: Some(rgba(0xdc, 0x8a, 0x78)),
        cursor_border: Some(rgba(0xdc, 0x8a, 0x78)),
        selection_fg: Some(rgba(0x4c, 0x4f, 0x69)),
        selection_bg: Some(rgba(0xcc, 0xd0, 0xda)),
        ansi: Some([
            rgba(0xcc, 0xd0, 0xda), // black
            rgba(0xd2, 0x0f, 0x39), // red
            rgba(0x40, 0xa0, 0x2b), // green
            rgba(0xdf, 0x8e, 0x1d), // yellow
            rgba(0x1e, 0x66, 0xf5), // blue
            rgba(0xea, 0x76, 0xcb), // purple/magenta
            rgba(0x04, 0xa5, 0xe5), // cyan
            rgba(0xac, 0xb0, 0xbe), // white
        ]),
        brights: Some([
            rgba(0xac, 0xb0, 0xbe), // bright black
            rgba(0xd2, 0x0f, 0x39), // bright red
            rgba(0x40, 0xa0, 0x2b), // bright green
            rgba(0xdf, 0x8e, 0x1d), // bright yellow
            rgba(0x1e, 0x66, 0xf5), // bright blue
            rgba(0xea, 0x76, 0xcb), // bright purple/magenta
            rgba(0x04, 0xa5, 0xe5), // bright cyan
            rgba(0x4c, 0x4f, 0x69), // bright white
        ]),
        indexed: HashMap::new(),
        scrollbar_thumb: Some(rgba(0xcc, 0xd0, 0xda)),
        split: Some(rgba(0xac, 0xb0, 0xbe)),
        tab_bar: None,
        visual_bell: None,
        compose_cursor: None,
        copy_mode_active_highlight_fg: None,
        copy_mode_active_highlight_bg: None,
        copy_mode_inactive_highlight_fg: None,
        copy_mode_inactive_highlight_bg: None,
        quick_select_label_fg: None,
        quick_select_label_bg: None,
        quick_select_match_fg: None,
        quick_select_match_bg: None,
        input_selector_label_fg: None,
        input_selector_label_bg: None,
        launcher_label_fg: None,
        launcher_label_bg: None,
    }
}

/// Returns the built-in Palette for a given ThemeName.
/// Returns None for ThemeName::Custom (caller supplies their own palette).
pub fn get_theme(name: &ThemeName) -> Option<Palette> {
    match name {
        ThemeName::Dark => Some(dark_theme()),
        ThemeName::Light => Some(light_theme()),
        ThemeName::Custom => None,
    }
}

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

/// Dark theme based on Catppuccin Mocha palette.
pub fn dark_theme() -> Palette {
    Palette {
        foreground: Some(rgba(0xcd, 0xd6, 0xf4)),
        background: Some(rgba(0x1e, 0x1e, 0x2e)),
        cursor_fg: Some(rgba(0x1e, 0x1e, 0x2e)),
        cursor_bg: Some(rgba(0xf5, 0xe0, 0xdc)),
        cursor_border: Some(rgba(0xf5, 0xe0, 0xdc)),
        selection_fg: Some(rgba(0xcd, 0xd6, 0xf4)),
        selection_bg: Some(rgba(0x45, 0x47, 0x5a)),
        ansi: Some([
            rgba(0x45, 0x47, 0x5a), // black
            rgba(0xf3, 0x8b, 0xa8), // red
            rgba(0xa6, 0xe3, 0xa1), // green
            rgba(0xf9, 0xe2, 0xaf), // yellow
            rgba(0x89, 0xb4, 0xfa), // blue
            rgba(0xcb, 0xa6, 0xf7), // purple/magenta
            rgba(0x89, 0xdc, 0xeb), // cyan
            rgba(0xba, 0xc2, 0xde), // white
        ]),
        brights: Some([
            rgba(0x58, 0x5b, 0x70), // bright black
            rgba(0xf3, 0x8b, 0xa8), // bright red
            rgba(0xa6, 0xe3, 0xa1), // bright green
            rgba(0xf9, 0xe2, 0xaf), // bright yellow
            rgba(0x89, 0xb4, 0xfa), // bright blue
            rgba(0xcb, 0xa6, 0xf7), // bright purple/magenta
            rgba(0x89, 0xdc, 0xeb), // bright cyan
            rgba(0xa6, 0xad, 0xc8), // bright white
        ]),
        indexed: HashMap::new(),
        scrollbar_thumb: Some(rgba(0x45, 0x47, 0x5a)),
        split: Some(rgba(0x58, 0x5b, 0x70)),
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

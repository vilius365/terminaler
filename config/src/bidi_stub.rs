//! Stub replacement for wezterm-bidi types used in config.
//! BIDI support was stripped from Terminaler.

use wezterm_dynamic::{FromDynamic, ToDynamic};

#[derive(Debug, Clone, Copy, PartialEq, Eq, FromDynamic, ToDynamic)]
pub enum ParagraphDirectionHint {
    LeftToRight,
    RightToLeft,
    AutoLeftToRight,
    AutoRightToLeft,
}

impl Default for ParagraphDirectionHint {
    fn default() -> Self {
        ParagraphDirectionHint::LeftToRight
    }
}

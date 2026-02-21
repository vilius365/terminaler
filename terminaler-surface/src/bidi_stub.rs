//! Stub replacement for terminaler-bidi.
//! Provides the same types but always assumes Left-to-Right direction.
//! BIDI (bidirectional text) support was stripped from Terminaler.

use core::ops::Range;
use terminaler_dynamic::{FromDynamic, ToDynamic};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    LeftToRight,
    #[allow(dead_code)]
    RightToLeft,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, FromDynamic, ToDynamic)]
pub enum ParagraphDirectionHint {
    #[default]
    LeftToRight,
    RightToLeft,
    AutoLeftToRight,
    AutoRightToLeft,
}

pub struct BidiRun {
    pub direction: Direction,
    pub range: Range<usize>,
    pub indices: Range<usize>,
}

impl ParagraphDirectionHint {
    /// Returns the resolved Direction for this paragraph hint.
    /// For Auto variants, defaults to the base direction.
    pub fn direction(&self) -> Direction {
        match self {
            ParagraphDirectionHint::RightToLeft | ParagraphDirectionHint::AutoRightToLeft => {
                Direction::RightToLeft
            }
            ParagraphDirectionHint::LeftToRight | ParagraphDirectionHint::AutoLeftToRight => {
                Direction::LeftToRight
            }
        }
    }
}

pub struct BidiContext;

impl BidiContext {
    pub fn new() -> Self {
        BidiContext
    }

    pub fn resolve_paragraph(&mut self, _paragraph: &[char], _hint: ParagraphDirectionHint) {
        // No-op: always treat as LTR
    }

    pub fn reordered_runs(&self, range: Range<usize>) -> Vec<BidiRun> {
        // Return a single LTR run covering the whole range
        vec![BidiRun {
            direction: Direction::LeftToRight,
            range: range.clone(),
            indices: range,
        }]
    }
}

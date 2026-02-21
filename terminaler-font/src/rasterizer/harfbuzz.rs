// Cairo was stripped from Terminaler (Windows-only build).
// The HarfbuzzRasterizer is kept as a stub; it always delegates to an error
// at runtime since COLR v1 rendering via cairo is not available.
// argb_to_rgba is kept because FreeTypeRasterizer uses it internally.

use crate::hbwrap::Font;
use crate::rasterizer::FAKE_ITALIC_SKEW;
use crate::units::PixelLength;
use crate::{FontRasterizer, ParsedFont, RasterizedGlyph};

pub struct HarfbuzzRasterizer {
    #[allow(dead_code)]
    font: Font,
}

impl HarfbuzzRasterizer {
    pub fn from_locator(parsed: &ParsedFont) -> anyhow::Result<Self> {
        let mut font = Font::from_locator(&parsed.handle)?;
        font.set_ot_funcs();

        if parsed.synthesize_italic {
            font.set_synthetic_slant(FAKE_ITALIC_SKEW as f32);
        }
        if parsed.synthesize_bold {
            font.set_synthetic_bold(0.02, 0.02, false);
        }

        Ok(Self { font })
    }
}

impl FontRasterizer for HarfbuzzRasterizer {
    fn rasterize_glyph(
        &self,
        _glyph_pos: u32,
        _size: f64,
        _dpi: u32,
    ) -> anyhow::Result<RasterizedGlyph> {
        // Cairo was stripped from Terminaler (Windows-only build).
        // COLR v1 glyph rendering via harfbuzz+cairo is not supported.
        anyhow::bail!(
            "HarfbuzzRasterizer is not supported in this build \
             (cairo was stripped for the Windows-only Terminaler fork)"
        )
    }
}

/// Convert ARGB pixel data (as produced by cairo) to RGBA.
/// Kept as a public function because FreeTypeRasterizer references it.
#[allow(dead_code)]
pub fn argb_to_rgba(data: &mut [u8]) {
    for pixel in data.chunks_exact_mut(4) {
        #[cfg(target_endian = "little")]
        let [b, g, r, a] = *pixel
        else {
            unreachable!()
        };
        #[cfg(target_endian = "big")]
        let [a, r, g, b] = *pixel
        else {
            unreachable!()
        };
        pixel.copy_from_slice(&[r, g, b, a]);
    }
}

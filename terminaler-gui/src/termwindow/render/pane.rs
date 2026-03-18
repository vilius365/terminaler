use crate::customglyph::{BlockAlpha, BlockCoord, Poly, PolyCommand, PolyStyle};
use crate::quad::{HeapQuadAllocator, QuadTrait, TripleLayerQuadAllocator};
use crate::selection::SelectionRange;
use crate::termwindow::box_model::*;
use crate::termwindow::render::{
    same_hyperlink, CursorProperties, LineQuadCacheKey, LineQuadCacheValue, LineToEleShapeCacheKey,
    RenderScreenLineParams,
};
use crate::termwindow::{ScrollHit, UIItem, UIItemType};
use ::window::bitmaps::TextureRect;
use ::window::DeadKeyStatus;
use anyhow::Context;
use config::VisualBellTarget;
use mux::pane::{PaneId, WithPaneLines};
use mux::renderable::{RenderableDimensions, StableCursorPosition};
use mux::tab::PositionedPane;
use std::rc::Rc;
use terminaler_font::FontConfiguration;
use ordered_float::NotNan;
use std::time::Instant;
use terminaler_dynamic::Value;
use terminaler_term::color::{ColorAttribute, ColorPalette};
use terminaler_term::{Line, StableRowIndex};
use window::color::LinearRgba;

// ---------------------------------------------------------------------------
// Overlay icon definitions.  Close and move-to-tab use poly_quad (single
// shapes with no thin gaps — they rasterise correctly at any size).  Layout
// icons use filled_rectangle (direct GPU quads — no rasterisation).
// ---------------------------------------------------------------------------

/// Shorthand for a filled Poly with Full intensity.
macro_rules! filled_poly {
    ($path:expr) => {
        Poly {
            path: $path,
            intensity: BlockAlpha::Full,
            style: PolyStyle::Fill,
        }
    };
}

/// Generates a 10-element PolyCommand array for a rounded rectangle.
macro_rules! rounded_rect_path {
    ($x:expr, $y:expr, $w:expr, $h:expr, $r:expr, $d:expr) => {
        [
            PolyCommand::MoveTo(BlockCoord::Frac($x + $r, $d), BlockCoord::Frac($y, $d)),
            PolyCommand::LineTo(BlockCoord::Frac($x + $w - $r, $d), BlockCoord::Frac($y, $d)),
            PolyCommand::QuadTo {
                control: (BlockCoord::Frac($x + $w, $d), BlockCoord::Frac($y, $d)),
                to: (BlockCoord::Frac($x + $w, $d), BlockCoord::Frac($y + $r, $d)),
            },
            PolyCommand::LineTo(BlockCoord::Frac($x + $w, $d), BlockCoord::Frac($y + $h - $r, $d)),
            PolyCommand::QuadTo {
                control: (BlockCoord::Frac($x + $w, $d), BlockCoord::Frac($y + $h, $d)),
                to: (BlockCoord::Frac($x + $w - $r, $d), BlockCoord::Frac($y + $h, $d)),
            },
            PolyCommand::LineTo(BlockCoord::Frac($x + $r, $d), BlockCoord::Frac($y + $h, $d)),
            PolyCommand::QuadTo {
                control: (BlockCoord::Frac($x, $d), BlockCoord::Frac($y + $h, $d)),
                to: (BlockCoord::Frac($x, $d), BlockCoord::Frac($y + $h - $r, $d)),
            },
            PolyCommand::LineTo(BlockCoord::Frac($x, $d), BlockCoord::Frac($y + $r, $d)),
            PolyCommand::QuadTo {
                control: (BlockCoord::Frac($x, $d), BlockCoord::Frac($y, $d)),
                to: (BlockCoord::Frac($x + $r, $d), BlockCoord::Frac($y, $d)),
            },
            PolyCommand::Close,
        ]
    };
}

// -- Close icon: two diagonal parallelograms forming an X (36×36) --
const ICON_CLOSE: &[Poly] = &[
    // Top-left → bottom-right diagonal
    filled_poly!(&[
        PolyCommand::MoveTo(BlockCoord::Frac(10, 36), BlockCoord::Frac(7, 36)),
        PolyCommand::LineTo(BlockCoord::Frac(29, 36), BlockCoord::Frac(26, 36)),
        PolyCommand::LineTo(BlockCoord::Frac(26, 36), BlockCoord::Frac(29, 36)),
        PolyCommand::LineTo(BlockCoord::Frac(7, 36), BlockCoord::Frac(10, 36)),
        PolyCommand::Close,
    ]),
    // Top-right → bottom-left diagonal
    filled_poly!(&[
        PolyCommand::MoveTo(BlockCoord::Frac(26, 36), BlockCoord::Frac(7, 36)),
        PolyCommand::LineTo(BlockCoord::Frac(7, 36), BlockCoord::Frac(26, 36)),
        PolyCommand::LineTo(BlockCoord::Frac(10, 36), BlockCoord::Frac(29, 36)),
        PolyCommand::LineTo(BlockCoord::Frac(29, 36), BlockCoord::Frac(10, 36)),
        PolyCommand::Close,
    ]),
];

// ---------------------------------------------------------------------------
// Toast toolbar constants (pub(crate) for hit-testing in mouseevent.rs)
// ---------------------------------------------------------------------------
pub(crate) const TOAST_BTN_SIZE: f32 = 30.0;
pub(crate) const TOAST_ICON_SIZE: f32 = 24.0;
pub(crate) const TOAST_ICON_INSET: f32 = (TOAST_BTN_SIZE - TOAST_ICON_SIZE) / 2.0;
pub(crate) const TOAST_GAP: f32 = 2.0;
pub(crate) const TOAST_PADDING: f32 = 4.0;
pub(crate) const TOAST_COUNT: usize = 9;
/// Total width: padding + 9*btn + 8*gap + padding
pub(crate) const TOAST_WIDTH: f32 =
    TOAST_PADDING * 2.0 + TOAST_COUNT as f32 * TOAST_BTN_SIZE + (TOAST_COUNT - 1) as f32 * TOAST_GAP;
/// Total height: padding + btn + padding
pub(crate) const TOAST_HEIGHT: f32 = TOAST_PADDING * 2.0 + TOAST_BTN_SIZE;
/// Collapsed trigger: padding + one button + padding
pub(crate) const TOAST_COLLAPSED_WIDTH: f32 = TOAST_PADDING * 2.0 + TOAST_BTN_SIZE;
/// Extra margin around expanded toast before auto-collapse
pub(crate) const TOAST_COLLAPSE_MARGIN: f32 = 8.0;
/// Minimum pane pixel width to show the toast (collapsed trigger fits in small panes)
pub(crate) const TOAST_MIN_PANE_WIDTH: f32 = TOAST_COLLAPSED_WIDTH + 10.0;
/// Minimum pane pixel height to show the toast
pub(crate) const TOAST_MIN_PANE_HEIGHT: f32 = TOAST_HEIGHT + 10.0;

pub(crate) const TOAST_BUTTON_NAMES: [&str; TOAST_COUNT] = [
    "close",
    "hsplit",
    "vsplit",
    "quad",
    "triple-right",
    "triple-bottom",
    "dev",
    "claude-code",
    "move-to-tab",
];

// ---------------------------------------------------------------------------
// Layout icons as fractional rectangles — rendered directly as GPU quads via
// filled_rectangle, bypassing the poly rasteriser entirely.  Each rect is
// (x_frac, y_frac, w_frac, h_frac) relative to the icon area.
// ---------------------------------------------------------------------------
const TOAST_LAYOUT_RECTS: [&[(f32, f32, f32, f32)]; 7] = [
    // hsplit: two vertical halves
    &[(0.0, 0.0, 0.46, 1.0), (0.54, 0.0, 0.46, 1.0)],
    // vsplit: two horizontal halves
    &[(0.0, 0.0, 1.0, 0.46), (0.0, 0.54, 1.0, 0.46)],
    // quad: four quadrants
    &[(0.0, 0.0, 0.46, 0.46), (0.54, 0.0, 0.46, 0.46),
      (0.0, 0.54, 0.46, 0.46), (0.54, 0.54, 0.46, 0.46)],
    // triple-right: big left + two right
    &[(0.0, 0.0, 0.62, 1.0), (0.71, 0.0, 0.29, 0.46), (0.71, 0.54, 0.29, 0.46)],
    // triple-bottom: big top + two bottom
    &[(0.0, 0.0, 1.0, 0.62), (0.0, 0.71, 0.46, 0.29), (0.54, 0.71, 0.46, 0.29)],
    // dev: left + right-top big + right-bottom small
    &[(0.0, 0.0, 0.46, 1.0), (0.54, 0.0, 0.46, 0.62), (0.54, 0.71, 0.46, 0.29)],
    // claude-code: big top + thin bottom
    &[(0.0, 0.0, 1.0, 0.71), (0.0, 0.79, 1.0, 0.21)],
];

// -- Move-to-tab icon: upward arrow (triangle head + rounded shaft) --
const ICON_MOVE_TAB: &[Poly] = &[
    // Arrowhead (triangle pointing up)
    filled_poly!(&[
        PolyCommand::MoveTo(BlockCoord::Frac(18, 36), BlockCoord::Frac(2, 36)),
        PolyCommand::LineTo(BlockCoord::Frac(32, 36), BlockCoord::Frac(18, 36)),
        PolyCommand::LineTo(BlockCoord::Frac(4, 36), BlockCoord::Frac(18, 36)),
        PolyCommand::Close,
    ]),
    // Shaft (rounded rect, overlaps into arrowhead)
    filled_poly!(&rounded_rect_path!(14, 16, 8, 18, 3, 36)),
];

impl crate::TermWindow {
    fn paint_pane_box_model(&mut self, pos: &PositionedPane) -> anyhow::Result<()> {
        let computed = self.build_pane(pos)?;
        let mut ui_items = computed.ui_items();
        self.ui_items.append(&mut ui_items);
        let gl_state = self.render_state.as_ref().unwrap();
        self.render_element(&computed, gl_state, None)
    }

    pub fn paint_pane(
        &mut self,
        pos: &PositionedPane,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        if self.config.use_box_model_render {
            return self.paint_pane_box_model(pos);
        }

        self.check_for_dirty_lines_and_invalidate_selection(&pos.pane);
        /*
        let zone = {
            let dims = pos.pane.get_dimensions();
            let position = self
                .get_viewport(pos.pane.pane_id())
                .unwrap_or(dims.physical_top);

            let zones = self.get_semantic_zones(&pos.pane);
            let idx = match zones.binary_search_by(|zone| zone.start_y.cmp(&position)) {
                Ok(idx) | Err(idx) => idx,
            };
            let idx = ((idx as isize) - 1).max(0) as usize;
            zones.get(idx).cloned()
        };
        */

        let global_cursor_fg = self.palette().cursor_fg;
        let global_cursor_bg = self.palette().cursor_bg;
        let config = self.config.clone();
        let palette = pos.pane.palette();

        let (padding_left, padding_top) = self.padding_left_top();

        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height()
                .context("tab_bar_pixel_height")?
        } else {
            0.
        };
        let (top_bar_height, bottom_bar_height) = if self.config.tab_bar_at_bottom {
            (0.0, tab_bar_height)
        } else {
            (tab_bar_height, 0.0)
        };

        let border = self.get_os_border();
        let top_pixel_y = top_bar_height + padding_top + border.top.get() as f32;

        let cursor = pos.pane.get_cursor_position();
        if pos.is_active {
            self.prev_cursor.update(&cursor);
        }

        let pane_id = pos.pane.pane_id();
        let current_viewport = self.get_viewport(pane_id);
        let dims = pos.pane.get_dimensions();

        let gl_state = self.render_state.as_ref().unwrap();

        let cursor_border_color = palette.cursor_border.to_linear();
        let foreground = palette.foreground.to_linear();
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();

        let window_is_transparent =
            !self.window_background.is_empty() || config.window_background_opacity != 1.0;

        let default_bg = palette
            .resolve_bg(ColorAttribute::Default)
            .to_linear()
            .mul_alpha(if window_is_transparent {
                0.
            } else {
                config.text_background_opacity
            });

        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;
        let sidebar_offset = self.sidebar_x_offset();
        let background_rect = {
            // We want to fill out to the edges of the splits
            let (x, width_delta) = if pos.left == 0 {
                (
                    sidebar_offset,
                    padding_left + border.left.get() as f32 + (cell_width / 2.0),
                )
            } else {
                (
                    padding_left + border.left.get() as f32 + sidebar_offset - (cell_width / 2.0)
                        + (pos.left as f32 * cell_width),
                    cell_width,
                )
            };

            let (y, height_delta) = if pos.top == 0 {
                (
                    (top_pixel_y - padding_top),
                    padding_top + (cell_height / 2.0),
                )
            } else {
                (
                    top_pixel_y + (pos.top as f32 * cell_height) - (cell_height / 2.0),
                    cell_height,
                )
            };
            euclid::rect(
                x,
                y,
                // Go all the way to the right edge if we're right-most
                if pos.left + pos.width >= self.terminal_size.cols as usize {
                    self.effective_right_edge() - x
                } else {
                    (pos.width as f32 * cell_width) + width_delta
                },
                // Go all the way to the bottom if we're bottom-most
                if pos.top + pos.height >= self.terminal_size.rows as usize {
                    self.dimensions.pixel_height as f32 - y
                } else {
                    (pos.height as f32 * cell_height) + height_delta as f32
                },
            )
        };

        if self.window_background.is_empty() {
            // Per-pane, palette-specified background

            let mut quad = self
                .filled_rectangle(
                    layers,
                    0,
                    background_rect,
                    palette
                        .background
                        .to_linear()
                        .mul_alpha(config.window_background_opacity),
                )
                .context("filled_rectangle")?;
            quad.set_hsv(if pos.is_active {
                None
            } else {
                Some(config.inactive_pane_hsb)
            });
        }

        // Notification ring: colored border for panes with unread notifications
        if !pos.is_active {
            let has_notif = {
                let per_pane = self.pane_state(pane_id);
                per_pane.notification_start.is_some()
            };
            if has_notif {
                let notif_color = LinearRgba::with_components(1.0, 0.4, 0.2, 0.8);
                let ring_width = 3.0f32;
                let br = background_rect;
                // Top edge
                self.filled_rectangle(
                    layers,
                    2,
                    euclid::rect(br.min_x(), br.min_y(), br.width(), ring_width),
                    notif_color,
                )
                .context("filled_rectangle notification top")?;
                // Bottom edge
                self.filled_rectangle(
                    layers,
                    2,
                    euclid::rect(br.min_x(), br.max_y() - ring_width, br.width(), ring_width),
                    notif_color,
                )
                .context("filled_rectangle notification bottom")?;
                // Left edge
                self.filled_rectangle(
                    layers,
                    2,
                    euclid::rect(br.min_x(), br.min_y(), ring_width, br.height()),
                    notif_color,
                )
                .context("filled_rectangle notification left")?;
                // Right edge
                self.filled_rectangle(
                    layers,
                    2,
                    euclid::rect(br.max_x() - ring_width, br.min_y(), ring_width, br.height()),
                    notif_color,
                )
                .context("filled_rectangle notification right")?;
            }
        }

        {
            // If the bell is ringing, we draw another background layer over the
            // top of this in the configured bell color
            if let Some(intensity) = self.get_intensity_if_bell_target_ringing(
                &pos.pane,
                &config,
                VisualBellTarget::BackgroundColor,
            ) {
                // target background color
                let LinearRgba(r, g, b, _) = config
                    .resolved_palette
                    .visual_bell
                    .as_deref()
                    .unwrap_or(&palette.foreground)
                    .to_linear();

                let background = if window_is_transparent {
                    // for transparent windows, we fade in the target color
                    // by adjusting its alpha
                    LinearRgba::with_components(r, g, b, intensity)
                } else {
                    // otherwise We'll interpolate between the background color
                    // and the the target color
                    let (r1, g1, b1, a) = palette
                        .background
                        .to_linear()
                        .mul_alpha(config.window_background_opacity)
                        .tuple();
                    LinearRgba::with_components(
                        r1 + (r - r1) * intensity,
                        g1 + (g - g1) * intensity,
                        b1 + (b - b1) * intensity,
                        a,
                    )
                };
                log::trace!("bell color is {:?}", background);

                let mut quad = self
                    .filled_rectangle(layers, 0, background_rect, background)
                    .context("filled_rectangle")?;

                quad.set_hsv(if pos.is_active {
                    None
                } else {
                    Some(config.inactive_pane_hsb)
                });
            }
        }

        // Draw a soft inner glow around the active pane to indicate focus.
        // Multiple concentric bands with decreasing alpha create a feathered
        // edge effect that works well with Windows 11 rounded corners.
        if pos.is_active {
            let base_color = window::color::LinearRgba(0.7, 0.7, 0.7, 1.0);
            let glow_width = 4.0_f32; // total glow depth in pixels
            let bands: u32 = 4;
            let band_size = glow_width / bands as f32;
            let peak_alpha = 0.55_f32;
            let r = &background_rect;
            let glow_x = r.origin.x;
            let glow_y = r.origin.y;
            let glow_w = r.size.width;
            let glow_h = r.size.height;

            for i in 0..bands {
                let inset = i as f32 * band_size;
                // Quadratic falloff: strongest at edges, fading toward center
                let t = 1.0 - (i as f32 / (bands - 1) as f32);
                let alpha = peak_alpha * t * t;
                let color = base_color.mul_alpha(alpha);

                let x = glow_x + inset;
                let y = glow_y + inset;
                let w = (glow_w - 2.0 * inset).max(0.0);
                let h = (glow_h - 2.0 * inset).max(0.0);

                if w < band_size * 2.0 || h < band_size * 2.0 {
                    break;
                }

                // Top edge
                self.filled_rectangle(
                    layers, 2, euclid::rect(x, y, w, band_size), color,
                )
                .context("active pane glow")?;
                // Bottom edge
                self.filled_rectangle(
                    layers, 2, euclid::rect(x, y + h - band_size, w, band_size), color,
                )
                .context("active pane glow")?;
                // Left edge (between top and bottom to avoid corner overlap)
                let inner_h = h - 2.0 * band_size;
                if inner_h > 0.0 {
                    self.filled_rectangle(
                        layers, 2, euclid::rect(x, y + band_size, band_size, inner_h), color,
                    )
                    .context("active pane glow")?;
                    // Right edge
                    self.filled_rectangle(
                        layers, 2,
                        euclid::rect(x + w - band_size, y + band_size, band_size, inner_h),
                        color,
                    )
                    .context("active pane glow")?;
                }
            }
        }

        // TODO: we only have a single scrollbar in a single position.
        // We only update it for the active pane, but we should probably
        // do a per-pane scrollbar.  That will require more extensive
        // changes to ScrollHit, mouse positioning, PositionedPane
        // and tab size calculation.
        if pos.is_active && self.show_scroll_bar && dims.scrollback_rows > dims.viewport_rows {
            let thumb_y_offset = top_bar_height as usize + border.top.get();

            let min_height = self.min_scroll_bar_height();

            let info = ScrollHit::thumb(
                &*pos.pane,
                current_viewport,
                self.dimensions.pixel_height.saturating_sub(
                    thumb_y_offset + border.bottom.get() + bottom_bar_height as usize,
                ),
                min_height as usize,
            );
            let abs_thumb_top = thumb_y_offset + info.top;
            let thumb_size = info.height;
            let color = window::color::LinearRgba(0.6, 0.6, 0.6, 1.0);

            // Adjust the scrollbar thumb position
            let config = &self.config;
            let padding = self.effective_right_padding(&config) as f32;

            let thumb_x = self.dimensions.pixel_width - padding as usize - border.right.get();

            // Register the scroll bar location
            self.ui_items.push(UIItem {
                x: thumb_x,
                width: padding as usize,
                y: thumb_y_offset,
                height: info.top,
                item_type: UIItemType::AboveScrollThumb,
            });
            self.ui_items.push(UIItem {
                x: thumb_x,
                width: padding as usize,
                y: abs_thumb_top,
                height: thumb_size,
                item_type: UIItemType::ScrollThumb,
            });
            self.ui_items.push(UIItem {
                x: thumb_x,
                width: padding as usize,
                y: abs_thumb_top + thumb_size,
                height: self
                    .dimensions
                    .pixel_height
                    .saturating_sub(abs_thumb_top + thumb_size),
                item_type: UIItemType::BelowScrollThumb,
            });

            let thumb_width = padding.min(6.0);
            self.filled_rectangle(
                layers,
                2,
                euclid::rect(
                    thumb_x as f32 + padding - thumb_width,
                    abs_thumb_top as f32,
                    thumb_width,
                    thumb_size as f32,
                ),
                color,
            )
            .context("filled_rectangle")?;
        }

        // --- Per-pane font scaling for text rendering ---
        // Background/borders above used base metrics to keep pane boundaries static.
        // For text rendering, swap to scaled fonts/metrics if this pane has a custom scale.
        let pane_scale = self.pane_font_scale(pane_id);
        let is_scaled = (pane_scale - 1.0).abs() > 0.001;

        // Pre-compute left_pixel_x with BASE metrics before any font swap
        let left_pixel_x = padding_left
            + border.left.get() as f32
            + sidebar_offset
            + (pos.left as f32 * self.render_metrics.cell_size.width as f32);

        let adjusted_top_pixel_y;
        let orig_fonts;
        let orig_metrics;

        if is_scaled {
            let base_cell_height = self.render_metrics.cell_size.height as f32;
            adjusted_top_pixel_y = top_pixel_y + pos.top as f32 * base_cell_height;

            orig_fonts = Some(Rc::clone(&self.fonts));
            orig_metrics = Some(self.render_metrics);

            let (pane_fonts, pane_metrics) = self.get_or_create_scaled_config(pane_scale);
            self.fonts = pane_fonts;
            self.render_metrics = pane_metrics;
        } else {
            adjusted_top_pixel_y = top_pixel_y;
            orig_fonts = None;
            orig_metrics = None;
        }

        // For scaled panes, shadow pos with top=0 (absorbed into adjusted_top_pixel_y)
        let _adjusted_pos;
        let pos: &PositionedPane = if is_scaled {
            _adjusted_pos = PositionedPane {
                index: pos.index,
                is_active: pos.is_active,
                is_zoomed: pos.is_zoomed,
                left: 0,
                top: 0,
                width: pos.width,
                height: pos.height,
                pixel_width: pos.pixel_width,
                pixel_height: pos.pixel_height,
                pane: pos.pane.clone(),
            };
            &_adjusted_pos
        } else {
            pos
        };

        let (selrange, rectangular) = {
            let sel = self.selection(pos.pane.pane_id());
            (sel.range.clone(), sel.rectangular)
        };

        let start = Instant::now();
        let selection_fg = palette.selection_fg.to_linear();
        let selection_bg = palette.selection_bg.to_linear();
        let cursor_fg = palette.cursor_fg.to_linear();
        let cursor_bg = palette.cursor_bg.to_linear();
        let cursor_is_default_color =
            palette.cursor_fg == global_cursor_fg && palette.cursor_bg == global_cursor_bg;

        let render_error;
        {
            let stable_range = match current_viewport {
                Some(top) => top..top + dims.viewport_rows as StableRowIndex,
                None => dims.physical_top..dims.physical_top + dims.viewport_rows as StableRowIndex,
            };

            pos.pane
                .apply_hyperlinks(stable_range.clone(), &self.config.hyperlink_rules);

            struct LineRender<'a, 'b> {
                term_window: &'a mut crate::TermWindow,
                selrange: Option<SelectionRange>,
                rectangular: bool,
                dims: RenderableDimensions,
                top_pixel_y: f32,
                left_pixel_x: f32,
                pos: &'a PositionedPane,
                pane_id: PaneId,
                cursor: &'a StableCursorPosition,
                palette: &'a ColorPalette,
                default_bg: LinearRgba,
                cursor_border_color: LinearRgba,
                selection_fg: LinearRgba,
                selection_bg: LinearRgba,
                cursor_fg: LinearRgba,
                cursor_bg: LinearRgba,
                foreground: LinearRgba,
                cursor_is_default_color: bool,
                white_space: TextureRect,
                filled_box: TextureRect,
                window_is_transparent: bool,
                layers: &'a mut TripleLayerQuadAllocator<'b>,
                error: Option<anyhow::Error>,
            }

            let mut render = LineRender {
                term_window: self,
                selrange,
                rectangular,
                dims,
                top_pixel_y: adjusted_top_pixel_y,
                left_pixel_x,
                pos,
                pane_id,
                cursor: &cursor,
                palette: &palette,
                cursor_border_color,
                selection_fg,
                selection_bg,
                cursor_fg,
                default_bg,
                cursor_bg,
                foreground,
                cursor_is_default_color,
                white_space,
                filled_box,
                window_is_transparent,
                layers,
                error: None,
            };

            impl<'a, 'b> LineRender<'a, 'b> {
                fn render_line(
                    &mut self,
                    stable_top: StableRowIndex,
                    line_idx: usize,
                    line: &&mut Line,
                ) -> anyhow::Result<()> {
                    let stable_row = stable_top + line_idx as StableRowIndex;
                    let selrange = self
                        .selrange
                        .map_or(0..0, |sel| sel.cols_for_row(stable_row, self.rectangular));
                    // Constrain to the pane width!
                    let selrange = selrange.start..selrange.end.min(self.dims.cols);

                    let (cursor, composing, password_input) = if self.cursor.y == stable_row {
                        (
                            Some(CursorProperties {
                                position: StableCursorPosition {
                                    y: 0,
                                    ..*self.cursor
                                },
                                dead_key_or_leader: self.term_window.dead_key_status
                                    != DeadKeyStatus::None
                                    || self.term_window.leader_is_active(),
                                cursor_fg: self.cursor_fg,
                                cursor_bg: self.cursor_bg,
                                cursor_border_color: self.cursor_border_color,
                                cursor_is_default_color: self.cursor_is_default_color,
                            }),
                            match (self.pos.is_active, &self.term_window.dead_key_status) {
                                (true, DeadKeyStatus::Composing(composing)) => {
                                    Some(composing.to_string())
                                }
                                _ => None,
                            },
                            if self.term_window.config.detect_password_input {
                                match self.pos.pane.get_metadata() {
                                    Value::Object(obj) => {
                                        match obj.get(&Value::String("password_input".to_string()))
                                        {
                                            Some(Value::Bool(b)) => *b,
                                            _ => false,
                                        }
                                    }
                                    _ => false,
                                }
                            } else {
                                false
                            },
                        )
                    } else {
                        (None, None, false)
                    };

                    let shape_hash = self.term_window.shape_hash_for_line(line);

                    let quad_key = LineQuadCacheKey {
                        pane_id: self.pane_id,
                        password_input,
                        pane_is_active: self.pos.is_active,
                        config_generation: self.term_window.config.generation(),
                        shape_generation: self.term_window.shape_generation,
                        quad_generation: self.term_window.quad_generation,
                        composing: composing.clone(),
                        selection: selrange.clone(),
                        cursor,
                        shape_hash,
                        top_pixel_y: NotNan::new(self.top_pixel_y).unwrap()
                            + (line_idx + self.pos.top) as f32
                                * self.term_window.render_metrics.cell_size.height as f32,
                        left_pixel_x: NotNan::new(self.left_pixel_x).unwrap(),
                        phys_line_idx: line_idx,
                        reverse_video: self.dims.reverse_video,
                    };

                    if let Some(cached_quad) =
                        self.term_window.line_quad_cache.borrow_mut().get(&quad_key)
                    {
                        let expired = cached_quad
                            .expires
                            .map(|i| Instant::now() >= i)
                            .unwrap_or(false);
                        let hover_changed = if cached_quad.invalidate_on_hover_change {
                            !same_hyperlink(
                                cached_quad.current_highlight.as_ref(),
                                self.term_window.current_highlight.as_ref(),
                            )
                        } else {
                            false
                        };
                        if !expired && !hover_changed {
                            cached_quad
                                .layers
                                .apply_to(self.layers)
                                .context("cached_quad.layers.apply_to")?;
                            self.term_window.update_next_frame_time(cached_quad.expires);
                            return Ok(());
                        }
                    }

                    let mut buf = HeapQuadAllocator::default();
                    let next_due = self.term_window.has_animation.borrow_mut().take();

                    let shape_key = LineToEleShapeCacheKey {
                        shape_hash,
                        shape_generation: quad_key.shape_generation,
                        composing: if self.cursor.y == stable_row && self.pos.is_active {
                            if let DeadKeyStatus::Composing(composing) =
                                &self.term_window.dead_key_status
                            {
                                Some((self.cursor.x, composing.to_string()))
                            } else {
                                None
                            }
                        } else {
                            None
                        },
                    };

                    let render_result = self
                        .term_window
                        .render_screen_line(
                            RenderScreenLineParams {
                                top_pixel_y: *quad_key.top_pixel_y,
                                left_pixel_x: self.left_pixel_x,
                                pixel_width: self.dims.cols as f32
                                    * self.term_window.render_metrics.cell_size.width as f32,
                                stable_line_idx: Some(stable_row),
                                line: &line,
                                selection: selrange.clone(),
                                cursor: &self.cursor,
                                palette: &self.palette,
                                dims: &self.dims,
                                config: &self.term_window.config,
                                cursor_border_color: self.cursor_border_color,
                                foreground: self.foreground,
                                is_active: self.pos.is_active,
                                pane: Some(&self.pos.pane),
                                selection_fg: self.selection_fg,
                                selection_bg: self.selection_bg,
                                cursor_fg: self.cursor_fg,
                                cursor_bg: self.cursor_bg,
                                cursor_is_default_color: self.cursor_is_default_color,
                                white_space: self.white_space,
                                filled_box: self.filled_box,
                                window_is_transparent: self.window_is_transparent,
                                default_bg: self.default_bg,
                                font: None,
                                style: None,
                                use_pixel_positioning: self
                                    .term_window
                                    .config
                                    .experimental_pixel_positioning,
                                render_metrics: self.term_window.render_metrics,
                                shape_key: Some(shape_key),
                                password_input,
                            },
                            &mut TripleLayerQuadAllocator::Heap(&mut buf),
                        )
                        .context("render_screen_line")?;

                    let expires = self.term_window.has_animation.borrow().as_ref().cloned();
                    self.term_window.update_next_frame_time(next_due);

                    buf.apply_to(self.layers)
                        .context("HeapQuadAllocator::apply_to")?;

                    let quad_value = LineQuadCacheValue {
                        layers: buf,
                        expires,
                        invalidate_on_hover_change: render_result.invalidate_on_hover_change,
                        current_highlight: if render_result.invalidate_on_hover_change {
                            self.term_window.current_highlight.clone()
                        } else {
                            None
                        },
                    };

                    self.term_window
                        .line_quad_cache
                        .borrow_mut()
                        .put(quad_key, quad_value);

                    Ok(())
                }
            }

            impl<'a, 'b> WithPaneLines for LineRender<'a, 'b> {
                fn with_lines_mut(&mut self, stable_top: StableRowIndex, lines: &mut [&mut Line]) {
                    for (line_idx, line) in lines.iter().enumerate() {
                        if let Err(err) = self.render_line(stable_top, line_idx, line) {
                            self.error.replace(err);
                            return;
                        }
                    }
                }
            }

            pos.pane.with_lines_mut(stable_range.clone(), &mut render);
            render_error = render.error.take();
        }

        // Restore original fonts/metrics if we swapped for per-pane scaling
        if let (Some(of), Some(om)) = (orig_fonts, orig_metrics) {
            self.fonts = of;
            self.render_metrics = om;
        }

        if let Some(error) = render_error {
            return Err(error).context("error while calling with_lines_mut");
        }

        /*
        if let Some(zone) = zone {
            // TODO: render a thingy to jump to prior prompt
        }
        */
        metrics::histogram!("paint_pane.lines").record(start.elapsed());
        log::trace!("lines elapsed {:?}", start.elapsed());

        Ok(())
    }

    pub fn build_pane(&mut self, pos: &PositionedPane) -> anyhow::Result<ComputedElement> {
        // First compute the bounds for the pane background

        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;
        let (padding_left, padding_top) = self.padding_left_top();
        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height()?
        } else {
            0.
        };
        let (top_bar_height, _bottom_bar_height) = if self.config.tab_bar_at_bottom {
            (0.0, tab_bar_height)
        } else {
            (tab_bar_height, 0.0)
        };

        let border = self.get_os_border();
        let top_pixel_y = top_bar_height + padding_top + border.top.get() as f32;
        let sidebar_off = self.sidebar_x_offset();

        // We want to fill out to the edges of the splits
        let (x, width_delta) = if pos.left == 0 {
            (
                sidebar_off,
                padding_left + border.left.get() as f32 + (cell_width / 2.0),
            )
        } else {
            (
                padding_left + border.left.get() as f32 + sidebar_off - (cell_width / 2.0)
                    + (pos.left as f32 * cell_width),
                cell_width,
            )
        };

        let (y, height_delta) = if pos.top == 0 {
            (
                (top_pixel_y - padding_top),
                padding_top + (cell_height / 2.0),
            )
        } else {
            (
                top_pixel_y + (pos.top as f32 * cell_height) - (cell_height / 2.0),
                cell_height,
            )
        };

        let background_rect = euclid::rect(
            x,
            y,
            // Go all the way to the right edge if we're right-most
            if pos.left + pos.width >= self.terminal_size.cols as usize {
                self.effective_right_edge() - x
            } else {
                (pos.width as f32 * cell_width) + width_delta
            },
            // Go all the way to the bottom if we're bottom-most
            if pos.top + pos.height >= self.terminal_size.rows as usize {
                self.dimensions.pixel_height as f32 - y
            } else {
                (pos.height as f32 * cell_height) + height_delta as f32
            },
        );

        // Bounds for the terminal cells
        let content_rect = euclid::rect(
            padding_left + border.left.get() as f32 + sidebar_off - (cell_width / 2.0)
                + (pos.left as f32 * cell_width),
            top_pixel_y + (pos.top as f32 * cell_height) - (cell_height / 2.0),
            pos.width as f32 * cell_width,
            pos.height as f32 * cell_height,
        );

        let palette = pos.pane.palette();

        // TODO: visual bell background layer
        // TODO: scrollbar

        Ok(ComputedElement {
            item_type: None,
            zindex: 0,
            bounds: background_rect,
            border: PixelDimension::default(),
            border_rect: background_rect,
            border_corners: None,
            colors: ElementColors {
                border: BorderColor::default(),
                bg: if self.window_background.is_empty() {
                    palette
                        .background
                        .to_linear()
                        .mul_alpha(self.config.window_background_opacity)
                        .into()
                } else {
                    InheritableColor::Inherited
                },
                text: InheritableColor::Inherited,
            },
            hover_colors: None,
            padding: background_rect,
            content_rect,
            baseline: 1.0,
            content: ComputedElementContent::Children(vec![]),
        })
    }

    pub fn paint_drop_zone_overlay(
        &mut self,
        layers: &mut crate::quad::TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        use crate::termwindow::DropZone;

        let tab_drag = match self.tab_drag.as_ref() {
            Some(td) if td.threshold_exceeded => td,
            _ => return Ok(()),
        };
        let target_pane_id = match tab_drag.target_pane {
            Some(id) => id,
            None => return Ok(()),
        };
        let zone = match tab_drag.target_zone {
            Some(z) => z,
            None => return Ok(()),
        };

        let panes = self.get_panes_to_render();
        let target_pos = match panes.iter().find(|p| p.pane.pane_id() == target_pane_id) {
            Some(pos) => pos,
            None => return Ok(()),
        };

        let (padding_left, padding_top) = self.padding_left_top();

        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height()
                .context("tab_bar_pixel_height")?
        } else {
            0.
        };
        let top_bar_height = if self.config.tab_bar_at_bottom {
            0.0
        } else {
            tab_bar_height
        };

        let border = self.get_os_border();
        let top_pixel_y = top_bar_height + padding_top + border.top.get() as f32;
        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;

        let sidebar_off = self.sidebar_x_offset();
        let pane_left = padding_left + border.left.get() as f32 + sidebar_off
            + (target_pos.left as f32 * cell_width);
        let pane_top = top_pixel_y + (target_pos.top as f32 * cell_height);
        let pane_width = target_pos.width as f32 * cell_width;
        let pane_height = target_pos.height as f32 * cell_height;

        let overlay_rect = match zone {
            DropZone::Left => euclid::rect(
                pane_left, pane_top,
                pane_width / 2.0, pane_height,
            ),
            DropZone::Right => euclid::rect(
                pane_left + pane_width / 2.0, pane_top,
                pane_width / 2.0, pane_height,
            ),
            DropZone::Top => euclid::rect(
                pane_left, pane_top,
                pane_width, pane_height / 2.0,
            ),
            DropZone::Bottom => euclid::rect(
                pane_left, pane_top + pane_height / 2.0,
                pane_width, pane_height / 2.0,
            ),
        };

        // Semi-transparent accent color (blue at 30% alpha)
        let color = window::color::LinearRgba(0.2, 0.4, 0.8, 0.3);

        self.filled_rectangle(layers, 2, overlay_rect, color)
            .context("paint_drop_zone_overlay")?;

        Ok(())
    }

    pub fn paint_pane_remove_overlay(
        &mut self,
        layers: &mut crate::quad::TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let lp = match self.pane_long_press.as_ref() {
            Some(lp) if lp.revealed => lp,
            _ => return Ok(()),
        };
        let target_pane_id = lp.pane_id;

        let panes = self.get_panes_to_render();
        let target_pos = match panes.iter().find(|p| p.pane.pane_id() == target_pane_id) {
            Some(pos) => pos,
            None => {
                // Pane no longer exists, clear state
                self.pane_long_press = None;
                return Ok(());
            }
        };

        let (padding_left, padding_top) = self.padding_left_top();

        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height()
                .context("tab_bar_pixel_height")?
        } else {
            0.
        };
        let top_bar_height = if self.config.tab_bar_at_bottom {
            0.0
        } else {
            tab_bar_height
        };

        let border = self.get_os_border();
        let top_pixel_y = top_bar_height + padding_top + border.top.get() as f32;
        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;

        let sidebar_off = self.sidebar_x_offset();
        let pane_left = padding_left + border.left.get() as f32 + sidebar_off
            + (target_pos.left as f32 * cell_width);
        let pane_top = top_pixel_y + (target_pos.top as f32 * cell_height);
        let pane_width = target_pos.width as f32 * cell_width;
        let pane_height = target_pos.height as f32 * cell_height;

        // Matte light grey overlay over the entire pane
        let dim_color = window::color::LinearRgba(0.85, 0.85, 0.85, 0.05);
        self.filled_rectangle(
            layers,
            2,
            euclid::rect(pane_left, pane_top, pane_width, pane_height),
            dim_color,
        )
        .context("paint_pane_remove_overlay dim")?;

        // 3×3 button grid centered in pane
        // Each button 60×60px, 8px gaps → grid 196×196
        let btn_size = 60.0f32;
        let gap = 8.0f32;
        let cols = 3;
        let rows = 3;
        let grid_w = (cols as f32) * btn_size + ((cols - 1) as f32) * gap;
        let grid_h = (rows as f32) * btn_size + ((rows - 1) as f32) * gap;
        let cx = pane_left + pane_width / 2.0;
        let cy = pane_top + pane_height / 2.0;
        let grid_left = cx - grid_w / 2.0;
        let grid_top = cy - grid_h / 2.0;

        let white = window::color::LinearRgba(1.0, 1.0, 1.0, 0.85);
        let close_bg = window::color::LinearRgba(0.85, 0.15, 0.15, 0.95);
        let layout_bg = window::color::LinearRgba(0.2, 0.25, 0.35, 0.95);
        let tab_bg = window::color::LinearRgba(0.15, 0.45, 0.65, 0.95);
        let icon_sz: euclid::Size2D<f32, window::PixelUnit> = euclid::size2(48.0, 48.0);
        let icon_area = 48.0f32;

        for row in 0..rows {
            for col in 0..cols {
                let idx = row * cols + col;
                let bl = grid_left + col as f32 * (btn_size + gap);
                let bt = grid_top + row as f32 * (btn_size + gap);

                // Button background (filled rect)
                let bg = match idx {
                    0 => close_bg,
                    8 => tab_bg,
                    _ => layout_bg,
                };
                self.filled_rectangle(
                    layers, 2,
                    euclid::rect(bl, bt, btn_size, btn_size),
                    bg,
                ).context("overlay button bg")?;

                // Icon foreground (6px padding → 48×48 interior)
                let ix = bl + 6.0;
                let iy = bt + 6.0;

                if idx == 0 {
                    // Close: X shape via poly_quad
                    self.poly_quad(
                        layers, 2, euclid::point2(ix, iy),
                        ICON_CLOSE, 1, icon_sz, white,
                    ).context("overlay close icon")?;
                } else if idx == 8 {
                    // Move-to-tab: arrow via poly_quad
                    self.poly_quad(
                        layers, 2, euclid::point2(ix, iy),
                        ICON_MOVE_TAB, 1, icon_sz, white,
                    ).context("overlay move-tab icon")?;
                } else if idx >= 1 && idx <= 7 {
                    // Layout icons: filled_rectangle quads
                    for &(xf, yf, wf, hf) in TOAST_LAYOUT_RECTS[idx - 1] {
                        self.filled_rectangle(
                            layers, 2,
                            euclid::rect(
                                ix + xf * icon_area,
                                iy + yf * icon_area,
                                wf * icon_area,
                                hf * icon_area,
                            ),
                            white,
                        ).context("overlay layout icon")?;
                    }
                }
            }
        }

        Ok(())
    }

    pub fn paint_toast_toolbar(
        &mut self,
        layers: &mut crate::quad::TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let hovered_id = match self.hovered_pane_id {
            Some(id) => id,
            None => return Ok(()),
        };

        // Don't show toast if long-press overlay is active on the same pane
        if self
            .pane_long_press
            .as_ref()
            .map_or(false, |lp| lp.revealed && lp.pane_id == hovered_id)
        {
            return Ok(());
        }

        // Don't show toast during tab drag
        if self
            .tab_drag
            .as_ref()
            .map_or(false, |td| td.threshold_exceeded)
        {
            return Ok(());
        }

        let panes = self.get_panes_to_render();
        let target_pos = match panes.iter().find(|p| p.pane.pane_id() == hovered_id) {
            Some(pos) => pos,
            None => return Ok(()),
        };

        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;
        let (padding_left, padding_top) = self.padding_left_top();

        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height()
                .context("tab_bar_pixel_height")?
        } else {
            0.
        };
        let top_bar_height = if self.config.tab_bar_at_bottom {
            0.0
        } else {
            tab_bar_height
        };

        let border = self.get_os_border();
        let top_pixel_y = top_bar_height + padding_top + border.top.get() as f32;

        // Compute true visual pane bounds (same logic as build_pane background_rect)
        let pos = target_pos;
        let sidebar_off = self.sidebar_x_offset();

        let (bg_x, _width_delta) = if pos.left == 0 {
            (sidebar_off, padding_left + border.left.get() as f32 + (cell_width / 2.0))
        } else {
            (
                padding_left + border.left.get() as f32 + sidebar_off - (cell_width / 2.0)
                    + (pos.left as f32 * cell_width),
                cell_width,
            )
        };
        let bg_y = if pos.top == 0 {
            top_pixel_y - padding_top
        } else {
            top_pixel_y + (pos.top as f32 * cell_height) - (cell_height / 2.0)
        };
        let bg_right = if pos.left + pos.width >= self.terminal_size.cols as usize {
            self.effective_right_edge()
        } else {
            bg_x + (pos.width as f32 * cell_width) + _width_delta
        };
        let bg_bottom = if pos.top + pos.height >= self.terminal_size.rows as usize {
            self.dimensions.pixel_height as f32
        } else {
            let height_delta = if pos.top == 0 {
                padding_top + (cell_height / 2.0)
            } else {
                cell_height
            };
            bg_y + (pos.height as f32 * cell_height) + height_delta
        };

        let pane_visual_width = bg_right - bg_x;
        let pane_visual_height = bg_bottom - bg_y;

        // Skip if pane too small
        if pane_visual_width < TOAST_MIN_PANE_WIDTH || pane_visual_height < TOAST_MIN_PANE_HEIGHT {
            return Ok(());
        }

        let is_expanded = self.toast_expanded_for == Some(hovered_id)
            && pane_visual_width >= TOAST_WIDTH + 10.0;

        let bg_color = window::color::LinearRgba(0.12, 0.14, 0.12, 0.55);
        let btn_bg = window::color::LinearRgba(1.0, 1.0, 1.0, 0.10);
        let white = window::color::LinearRgba(1.0, 1.0, 1.0, 0.92);
        let icon_sz: euclid::Size2D<f32, window::PixelUnit> =
            euclid::size2(TOAST_ICON_SIZE, TOAST_ICON_SIZE);

        // Offset toast from the top edge of the pane
        let toast_top_offset = 10.0f32;

        if is_expanded {
            // --- Expanded: full 9-button toolbar ---
            let toast_left = bg_right - TOAST_WIDTH;
            let toast_top = bg_y + toast_top_offset;

            self.filled_rectangle(
                layers, 2,
                euclid::rect(toast_left, toast_top, TOAST_WIDTH, TOAST_HEIGHT),
                bg_color,
            ).context("toast toolbar bg")?;

            let close_btn_bg = window::color::LinearRgba(0.85, 0.2, 0.2, 0.45);

            for i in 0..TOAST_COUNT {
                let bx = toast_left + TOAST_PADDING + i as f32 * (TOAST_BTN_SIZE + TOAST_GAP);
                let by = toast_top + TOAST_PADDING;

                let bg = if i == 0 { close_btn_bg } else { btn_bg };
                self.filled_rectangle(
                    layers, 2,
                    euclid::rect(bx, by, TOAST_BTN_SIZE, TOAST_BTN_SIZE),
                    bg,
                ).context("toast btn bg")?;

                let ix = bx + TOAST_ICON_INSET;
                let iy = by + TOAST_ICON_INSET;

                if i == 0 {
                    self.poly_quad(
                        layers, 2, euclid::point2(ix, iy),
                        ICON_CLOSE, 1, icon_sz, white,
                    ).context("toast close icon")?;
                } else if i == 8 {
                    self.poly_quad(
                        layers, 2, euclid::point2(ix, iy),
                        ICON_MOVE_TAB, 1, icon_sz, white,
                    ).context("toast move-tab icon")?;
                } else {
                    for &(xf, yf, wf, hf) in TOAST_LAYOUT_RECTS[i - 1] {
                        self.filled_rectangle(
                            layers, 2,
                            euclid::rect(
                                ix + xf * TOAST_ICON_SIZE,
                                iy + yf * TOAST_ICON_SIZE,
                                wf * TOAST_ICON_SIZE,
                                hf * TOAST_ICON_SIZE,
                            ),
                            white,
                        ).context("toast layout icon")?;
                    }
                }
            }
        } else {
            // --- Collapsed: single trigger pill (quad layout icon) ---
            let pill_left = bg_right - TOAST_COLLAPSED_WIDTH;
            let pill_top = bg_y + toast_top_offset;

            self.filled_rectangle(
                layers, 2,
                euclid::rect(pill_left, pill_top, TOAST_COLLAPSED_WIDTH, TOAST_HEIGHT),
                bg_color,
            ).context("toast trigger bg")?;

            let bx = pill_left + TOAST_PADDING;
            let by = pill_top + TOAST_PADDING;
            self.filled_rectangle(
                layers, 2,
                euclid::rect(bx, by, TOAST_BTN_SIZE, TOAST_BTN_SIZE),
                btn_bg,
            ).context("toast trigger btn bg")?;

            // Quad layout icon (index 2 = TOAST_LAYOUT_RECTS[2])
            let ix = bx + TOAST_ICON_INSET;
            let iy = by + TOAST_ICON_INSET;
            for &(xf, yf, wf, hf) in TOAST_LAYOUT_RECTS[2] {
                self.filled_rectangle(
                    layers, 2,
                    euclid::rect(
                        ix + xf * TOAST_ICON_SIZE,
                        iy + yf * TOAST_ICON_SIZE,
                        wf * TOAST_ICON_SIZE,
                        hf * TOAST_ICON_SIZE,
                    ),
                    white,
                ).context("toast trigger icon")?;
            }
        }

        Ok(())
    }

}

//! Brief bar: a fixed-height strip across the very top of the window with one
//! cell per live Claude Code session (status dot, name, goal, current step).
//! The strip's height is reserved through `get_os_border_impl` (borders.rs);
//! this module only paints it.

use crate::brief_bar::{BriefRow, BriefSnapshot};
use crate::termwindow::box_model::*;
use crate::termwindow::UIItemType;
use crate::termwindow::render::tab_sidebar::SidebarTheme;
use crate::utilsprites::RenderMetrics;
use config::Dimension;
use std::rc::Rc;
use terminaler_font::LoadedFont;
use window::color::LinearRgba;

/// Cells narrower than this many characters are not worth drawing; the grid
/// caps its columns and folds the overflow into a `+K more` cell instead.
const MIN_CELL_CHARS: usize = 28;
/// Proportional-glyph advance as a fraction of the metrics reference cell.
const AVG_ADVANCE_FRAC: f32 = 0.55;
/// Horizontal text padding inside a cell, in pixels.
const CELL_PAD_X: f32 = 8.;

/// How `n` sessions are arranged in a `rows`-row grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridPlan {
    pub cols: usize,
    /// Session cells drawn.
    pub shown: usize,
    /// Sessions folded into the trailing `+K more` cell (0 = no such cell).
    pub more: usize,
}

impl GridPlan {
    pub fn cell_count(&self) -> usize {
        self.shown + usize::from(self.more > 0)
    }
}

/// Pack `n` cells row-major, `ceil(n / rows)` columns, unless that would make
/// cells narrower than `MIN_CELL_CHARS`; then cap the columns and fold the
/// overflow into a final `+K more` cell.
pub fn plan_grid(n: usize, rows: usize, width_chars: usize) -> GridPlan {
    let rows = rows.max(1);
    if n == 0 {
        return GridPlan {
            cols: 0,
            shown: 0,
            more: 0,
        };
    }
    let mut cols = (n + rows - 1) / rows;
    if width_chars / cols < MIN_CELL_CHARS {
        cols = (width_chars / MIN_CELL_CHARS).max(1);
    }
    let capacity = cols * rows;
    if n > capacity {
        let shown = capacity - 1;
        GridPlan {
            cols,
            shown,
            more: n - shown,
        }
    } else {
        GridPlan {
            cols,
            shown: n,
            more: 0,
        }
    }
}

/// Text of one cell after fitting a character budget.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CellText {
    pub name: String,
    pub goal: String,
    pub now: String,
}

fn truncate(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        String::new()
    } else if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let head: String = s.chars().take(max_chars - 1).collect();
        format!("{}\u{2026}", head)
    }
}

/// Fit name, goal and now into `budget` characters (the dot is excluded).
/// Truncation eats from the right: `now` is dropped before `goal`, and `goal`
/// before `name`. Separators cost one char before the goal and 3 (` — `)
/// before the step.
pub fn fit_cell(name: &str, goal: Option<&str>, now: Option<&str>, budget: usize) -> CellText {
    let name_t = truncate(name, budget);
    let mut used = name_t.chars().count();
    let mut out = CellText {
        name: name_t,
        ..Default::default()
    };
    let goal = match goal {
        Some(g) if !g.is_empty() => g,
        _ => return out,
    };
    // One space after the name, and at least one visible goal char.
    if budget < used + 2 {
        return out;
    }
    let goal_t = truncate(goal, budget - used - 1);
    used += 1 + goal_t.chars().count();
    let goal_full = goal_t == goal;
    out.goal = goal_t;
    if let Some(now) = now.filter(|n| !n.is_empty()) {
        // ` — ` plus at least one visible char, and only once goal fits whole.
        if goal_full && budget >= used + 4 {
            out.now = truncate(now, budget - used - 3);
        }
    }
    out
}

fn dot_color(row: &BriefRow, theme: &SidebarTheme) -> LinearRgba {
    if row.goal.is_none() || row.inferred == Some(true) {
        theme.text_tertiary
    } else if row.stale == Some(true) {
        theme.accent_yellow
    } else {
        theme.accent_green
    }
}

fn text_colors(c: LinearRgba) -> ElementColors {
    ElementColors {
        border: BorderColor::default(),
        bg: InheritableColor::Inherited,
        text: c.into(),
    }
}

fn part(font: &Rc<LoadedFont>, text: String, color: LinearRgba) -> Element {
    Element::new(font, ElementContent::Text(text))
        .display(DisplayType::Inline)
        .colors(text_colors(color))
}

fn stale_minutes(snap: &BriefSnapshot) -> Option<u64> {
    if snap.last_error.is_some() && !snap.rows.is_empty() {
        snap.last_success
            .map(|t| (t.elapsed().as_secs() / 60).max(1))
    } else {
        None
    }
}

/// Everything the painted output depends on, so the cached elements are
/// rebuilt exactly when something visible changed.
fn fingerprint(
    snap: &BriefSnapshot,
    targets: &[Option<(String, String)>],
    rows: usize,
    width: f32,
    height: f32,
    dpi: usize,
) -> String {
    let mut fp = format!("{}x{}@{}r{}|", width as i64, height as i64, dpi, rows);
    for r in &snap.rows {
        fp.push_str(&format!(
            "{:?}\u{1}{:?}\u{1}{:?}\u{1}{:?}\u{1}{:?}\n",
            r.name, r.goal, r.now, r.inferred, r.stale
        ));
    }
    fp.push_str(&format!("targets={:?}|", targets));
    fp.push_str(&format!(
        "err={:?}|ok={}|stale={:?}",
        snap.last_error,
        snap.last_success.is_some(),
        stale_minutes(snap)
    ));
    fp
}

impl crate::TermWindow {
    /// Height of the strip in pixels, 0 when the feature is off.
    pub fn brief_bar_height(&self) -> f32 {
        Self::brief_bar_height_for(&self.config, &self.render_metrics)
    }

    pub fn brief_bar_height_for(
        config: &config::ConfigHandle,
        render_metrics: &RenderMetrics,
    ) -> f32 {
        match config.brief_bar.as_ref() {
            Some(b) if b.enabled => b.strip_height_px(render_metrics.cell_size.height as f32),
            _ => 0.,
        }
    }

    pub fn paint_brief_bar(
        &mut self,
        layers: &mut crate::quad::TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        use anyhow::Context;

        let strip_h = self.brief_bar_height();
        if strip_h <= 0. {
            self.brief_bar_cache = None;
            return Ok(());
        }
        crate::brief_bar::ensure_running();

        let cfg = self.config.brief_bar.clone().unwrap_or_default();
        let rows = cfg.effective_rows() as usize;
        let width = self.dimensions.pixel_width as f32;
        let border = self.get_os_border();
        let y0 = (border.top.get() as f32 - strip_h).max(0.);
        let theme = SidebarTheme::load();

        let snap = crate::brief_bar::snapshot();
        let tmux_cfg = self.config.tmux.clone();
        let tmux_snaps = crate::tmux_discovery::snapshot();
        let targets: Vec<Option<(String, String)>> = snap
            .rows
            .iter()
            .map(|r| crate::brief_bar::match_session(r, tmux_cfg.as_ref(), &tmux_snaps))
            .collect();
        let fp = fingerprint(&snap, &targets, rows, width, strip_h, self.dimensions.dpi);
        let rebuild = self
            .brief_bar_cache
            .as_ref()
            .map_or(true, |(old, _)| *old != fp);
        if rebuild {
            let elements = self
                .build_brief_bar(&snap, &targets, rows, width, y0, strip_h, &theme)
                .context("build_brief_bar")?;
            self.brief_bar_cache = Some((fp, elements));
        }

        // Background, bottom divider, and cell separators.
        self.filled_rectangle(
            layers,
            1,
            euclid::rect(0., y0, width, strip_h),
            theme.bg_surface,
        )
        .context("brief bar background")?;
        self.filled_rectangle(
            layers,
            1,
            euclid::rect(0., y0 + strip_h - 1., width, 1.),
            theme.border_default,
        )
        .context("brief bar divider")?;

        // Strip-wide inert item first: hit-testing walks items in reverse, so
        // the clickable cells pushed after it win. Clicks anywhere in the
        // strip are swallowed. Cell rects come from the elements computed at
        // paint time, never recomputed in the mouse handler.
        self.ui_items.push(crate::termwindow::UIItem {
            x: 0,
            y: y0 as usize,
            width: width as usize,
            height: strip_h as usize,
            item_type: crate::termwindow::UIItemType::BriefBar,
        });
        if let Some((_, elements)) = self.brief_bar_cache.as_ref() {
            let gl_state = self.render_state.as_ref().unwrap();
            for el in elements {
                self.render_element(el, gl_state, None)?;
                self.ui_items.extend(el.ui_items());
            }
        }
        Ok(())
    }

    fn build_brief_bar(
        &self,
        snap: &BriefSnapshot,
        targets: &[Option<(String, String)>],
        rows: usize,
        width: f32,
        y0: f32,
        strip_h: f32,
        theme: &SidebarTheme,
    ) -> anyhow::Result<Vec<ComputedElement>> {
        let font = self.fonts.sidebar_font_at(self.rail_font_size())?;
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());
        let line_h = self.render_metrics.cell_size.height as f32;
        let ctx = self.brief_layout_ctx(&metrics, width, strip_h);
        let avg_adv = (metrics.cell_size.width as f32 * AVG_ADVANCE_FRAC).max(1.);
        let chars_for = |px: f32| ((px - 2. * CELL_PAD_X).max(0.) / avg_adv) as usize;

        let mut out: Vec<ComputedElement> = vec![];
        let place = |parts: Vec<Element>,
                     x: f32,
                     row: usize,
                     w: f32,
                     target: Option<&(String, String)>|
         -> anyhow::Result<ComputedElement> {
            let mut el = Element::new(&font, ElementContent::Children(parts))
                .display(DisplayType::Block)
                .padding(BoxDimension {
                    left: Dimension::Pixels(CELL_PAD_X),
                    right: Dimension::Pixels(CELL_PAD_X),
                    top: Dimension::Pixels(0.),
                    bottom: Dimension::Pixels(0.),
                })
                .colors(text_colors(theme.text_primary))
                .min_width(Some(Dimension::Pixels(w)));
            if let Some((box_name, session)) = target {
                el = el
                    .item_type(UIItemType::BriefBarSession {
                        box_name: box_name.clone(),
                        session: session.clone(),
                    })
                    .hover_colors(Some(ElementColors {
                        border: BorderColor::default(),
                        bg: theme.bg_elevated.into(),
                        text: theme.text_primary.into(),
                    }));
            }
            let mut computed = self.compute_element(&ctx, &el)?;
            computed.translate(euclid::vec2(x, y0 + 2. + row as f32 * line_h));
            Ok(computed)
        };

        if snap.rows.is_empty() {
            let (dot, text) = match &snap.last_error {
                Some(err) if snap.last_success.is_none() => (
                    theme.accent_red,
                    format!("brief feed: {}", truncate(err, chars_for(width))),
                ),
                Some(err) => (
                    theme.accent_red,
                    format!("brief feed: {}", truncate(err, chars_for(width))),
                ),
                None => (theme.text_tertiary, "no live sessions".to_string()),
            };
            out.push(place(
                vec![
                    part(&font, "\u{25cf} ".to_string(), dot),
                    part(&font, text, theme.text_tertiary),
                ],
                0.,
                0,
                width,
                None,
            )?);
            return Ok(out);
        }

        let width_chars = (width / avg_adv) as usize;
        let plan = plan_grid(snap.rows.len(), rows, width_chars);
        let cell_w = width / plan.cols as f32;
        let budget = chars_for(cell_w).saturating_sub(2);

        for i in 0..plan.cell_count() {
            let (r, c) = (i / plan.cols, i % plan.cols);
            let x = c as f32 * cell_w;
            let parts = if i < plan.shown {
                let row = &snap.rows[i];
                let name = row.name.as_deref().unwrap_or("?");
                let has_goal = row.goal.is_some();
                let fitted = if has_goal {
                    fit_cell(name, row.goal.as_deref(), row.now.as_deref(), budget)
                } else {
                    fit_cell(name, Some("no brief"), None, budget)
                };
                let mut parts = vec![
                    part(&font, "\u{25cf} ".to_string(), dot_color(row, theme)),
                    part(&font, fitted.name, theme.accent_orange),
                ];
                if !fitted.goal.is_empty() {
                    let color = if has_goal {
                        theme.text_primary
                    } else {
                        theme.text_tertiary
                    };
                    parts.push(part(&font, format!(" {}", fitted.goal), color));
                }
                if !fitted.now.is_empty() {
                    parts.push(part(
                        &font,
                        format!(" \u{2014} {}", fitted.now),
                        theme.text_tertiary,
                    ));
                }
                parts
            } else {
                vec![part(
                    &font,
                    format!("+{} more", plan.more),
                    theme.text_tertiary,
                )]
            };
            let target = if i < plan.shown {
                targets.get(i).and_then(|t| t.as_ref())
            } else {
                None
            };
            out.push(place(parts, x, r, cell_w, target)?);
            // Thin separator before every cell but the first in a row.
            // (Drawn as a glyph-free text-less element would need a quad
            // allocator; a dim bar glyph keeps this inside the element path.)
            if c > 0 {
                let sep = vec![part(&font, "\u{2502}".to_string(), theme.border_default)];
                let el = Element::new(&font, ElementContent::Children(sep))
                    .display(DisplayType::Block)
                    .colors(text_colors(theme.border_default));
                let mut computed = self.compute_element(&ctx, &el)?;
                computed.translate(euclid::vec2(x - 3., y0 + 2. + r as f32 * line_h));
                out.push(computed);
            }
        }

        if let Some(mins) = stale_minutes(snap) {
            let label = format!(" \u{26a0} stale {}m ", mins);
            let w = label.chars().count() as f32 * avg_adv + 2. * CELL_PAD_X;
            let el = Element::new(&font, ElementContent::Text(label))
                .display(DisplayType::Block)
                .padding(BoxDimension {
                    left: Dimension::Pixels(CELL_PAD_X),
                    right: Dimension::Pixels(CELL_PAD_X),
                    top: Dimension::Pixels(0.),
                    bottom: Dimension::Pixels(0.),
                })
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: theme.bg_surface.into(),
                    text: theme.accent_red.into(),
                })
                .min_width(Some(Dimension::Pixels(w)));
            let mut computed = self.compute_element(&ctx, &el)?;
            computed.translate(euclid::vec2((width - w - 4.).max(0.), y0 + 2.));
            out.push(computed);
        }
        Ok(out)
    }

    fn brief_layout_ctx<'a>(
        &'a self,
        metrics: &'a RenderMetrics,
        max_w: f32,
        max_h: f32,
    ) -> LayoutContext<'a> {
        let dpi = self.dimensions.dpi as f32;
        LayoutContext {
            width: config::DimensionContext {
                dpi,
                pixel_max: max_w,
                pixel_cell: metrics.cell_size.width as f32,
            },
            height: config::DimensionContext {
                dpi,
                pixel_max: max_h,
                pixel_cell: metrics.cell_size.height as f32,
            },
            bounds: euclid::rect(0., 0., max_w, max_h),
            metrics,
            gl_state: self.render_state.as_ref().unwrap(),
            // Not 0: paint_impl holds zindex 0's quad layers mapped for the
            // whole frame, so rendering an element there panics with
            // already-borrowed. The sidebar uses 10 for the same reason.
            zindex: 10,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_empty_and_single() {
        assert_eq!(plan_grid(0, 2, 200).cell_count(), 0);
        let p = plan_grid(1, 2, 200);
        assert_eq!((p.cols, p.shown, p.more), (1, 1, 0));
    }

    #[test]
    fn grid_packs_row_major_when_wide() {
        let p = plan_grid(5, 2, 200);
        assert_eq!((p.cols, p.shown, p.more), (3, 5, 0));
        let p = plan_grid(7, 2, 200);
        assert_eq!((p.cols, p.shown, p.more), (4, 7, 0));
    }

    #[test]
    fn grid_folds_overflow_into_more() {
        // 20 sessions would need 10 cols (20 chars each): cap to 7, 14 slots.
        let p = plan_grid(20, 2, 200);
        assert_eq!((p.cols, p.shown, p.more), (7, 13, 7));
        assert_eq!(p.cell_count(), 14);
        // Narrow window: 2 cols x 2 rows = 4 slots for 5 sessions.
        let p = plan_grid(5, 2, 60);
        assert_eq!((p.cols, p.shown, p.more), (2, 3, 2));
        // Very narrow: still one column.
        let p = plan_grid(5, 2, 10);
        assert_eq!((p.cols, p.shown, p.more), (1, 1, 4));
    }

    #[test]
    fn fit_drops_now_before_goal_before_name() {
        let full = fit_cell("notch", Some("Slim the buttons"), Some("Wrapping up"), 60);
        assert_eq!(full.goal, "Slim the buttons");
        assert_eq!(full.now, "Wrapping up");
        // Goal fits (22 chars) but not the step.
        let t = fit_cell("notch", Some("Slim the buttons"), Some("Wrapping up"), 24);
        assert_eq!(t.goal, "Slim the buttons");
        assert_eq!(t.now, "");
        // Goal truncated, no step.
        let t = fit_cell("notch", Some("Slim the buttons"), Some("Wrapping up"), 14);
        assert!(t.goal.ends_with('\u{2026}'));
        assert_eq!(t.now, "");
        // Only the name survives.
        let t = fit_cell("notch", Some("Slim"), None, 5);
        assert_eq!((t.name.as_str(), t.goal.as_str()), ("notch", ""));
        let t = fit_cell("notch", Some("Slim"), None, 3);
        assert_eq!(t.name, "no\u{2026}");
    }
}

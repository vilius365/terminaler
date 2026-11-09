use crate::termwindow::{RenderFrame, TermWindowNotif};
use ::window::bitmaps::atlas::OutOfTextureSpace;
use ::window::WindowOps;
use anyhow::Context;
use smol::Timer;
use std::time::{Duration, Instant};
use terminaler_font::ClearShapeCache;
use crate::quad::{HeapQuadAllocator, TripleLayerQuadAllocator};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowImage {
    Yes,
    Scale(usize),
    No,
}

impl crate::TermWindow {
    pub fn paint_impl(&mut self, frame: &mut RenderFrame) {
        self.num_frames += 1;
        // If nothing on screen needs animating, then we can avoid
        // invalidating as frequently
        *self.has_animation.borrow_mut() = None;
        // Start with the assumption that we should allow images to render
        self.allow_images = AllowImage::Yes;

        let start = Instant::now();

        {
            let diff = start.duration_since(self.last_fps_check_time);
            if diff > Duration::from_secs(1) {
                let seconds = diff.as_secs_f32();
                self.fps = self.num_frames as f32 / seconds;
                self.num_frames = 0;
                self.last_fps_check_time = start;
            }
        }

        self.check_claude_idle_notifications();

        'pass: for pass in 0.. {
            match self.paint_pass() {
                Ok(_) => match self.render_state.as_mut().unwrap().allocated_more_quads() {
                    Ok(allocated) => {
                        if !allocated {
                            break 'pass;
                        }
                        if self.config.log_animation_frames {
                            log::info!(target: "anim_frames", "  repaint pass {}: grew quad buffers", pass + 1);
                        }
                        self.invalidate_fancy_tab_bar();
                        self.invalidate_tab_sidebar();
                        self.brief_bar_cache = None;
                        self.invalidate_modal();
                    }
                    Err(err) => {
                        log::error!("{:#}", err);
                        break 'pass;
                    }
                },
                Err(err) => {
                    if let Some(&OutOfTextureSpace {
                        size: Some(size),
                        current_size,
                    }) = err.root_cause().downcast_ref::<OutOfTextureSpace>()
                    {
                        log::debug!(
                            target: "anim_frames",
                            "  repaint pass {}: glyph atlas full at {} (wants {}); {}",
                            pass + 1,
                            current_size,
                            size,
                            if pass == 0 { "clearing it" } else { "growing it" }
                        );
                        // Held pictures point into the old atlas.
                        self.pane_holds.clear();
                        self.pane_hold_quads.clear();
                        self.pane_snapshots.clear();
                        let result = if pass == 0 {
                            // Let's try clearing out the atlas and trying again
                            // self.clear_texture_atlas()
                            log::trace!("recreate_texture_atlas");
                            self.recreate_texture_atlas(Some(current_size))
                        } else {
                            log::trace!("grow texture atlas to {}", size);
                            self.recreate_texture_atlas(Some(size))
                        };
                        self.invalidate_fancy_tab_bar();
                        self.invalidate_tab_sidebar();
                        self.brief_bar_cache = None;
                        self.invalidate_modal();

                        if let Err(err) = result {
                            self.allow_images = match self.allow_images {
                                AllowImage::Yes => AllowImage::Scale(2),
                                AllowImage::Scale(2) => AllowImage::Scale(4),
                                AllowImage::Scale(4) => AllowImage::Scale(8),
                                AllowImage::Scale(8) => AllowImage::No,
                                AllowImage::No | _ => {
                                    log::error!(
                                        "Failed to {} texture: {}",
                                        if pass == 0 { "clear" } else { "resize" },
                                        err
                                    );
                                    break 'pass;
                                }
                            };

                            log::info!(
                                "Not enough texture space ({:#}); \
                                     will retry render with {:?}",
                                err,
                                self.allow_images,
                            );
                        }
                    } else if err.root_cause().downcast_ref::<ClearShapeCache>().is_some() {
                        if self.config.log_animation_frames {
                            log::info!(target: "anim_frames", "  repaint pass {}: shape cache cleared", pass + 1);
                        }
                        self.invalidate_fancy_tab_bar();
                        self.invalidate_tab_sidebar();
                        self.brief_bar_cache = None;
                        self.invalidate_modal();
                        self.shape_generation += 1;
                        self.shape_cache.borrow_mut().clear();
                        self.line_to_ele_shape_cache.borrow_mut().clear();
                    } else {
                        log::error!("paint_pass failed: {:#}", err);
                        break 'pass;
                    }
                }
            }
        }
        if let Some((index, delta)) = self.pending_split_resize.take() {
            let now = Instant::now();
            if let Some(captured) = self.pane_hold_capture.take() {
                for (id, (quads, rect)) in captured {
                    self.pane_holds.start(id, rect, now);
                    self.pane_hold_quads.insert(id, quads);
                }
            }
            if let Some(tab) = mux::Mux::get().get_active_tab_for_window(self.mux_window_id) {
                log::info!("split drag applied: move split {} by {} cells", index, delta);
                tab.resize_split_by(index, delta);
            }
            if let Some(window) = self.window.as_ref() {
                window.invalidate();
            }
        }
        if let Some(pane_id) = self.pending_focus_layout.take() {
            if let Some(captured) = self.pane_hold_capture.take() {
                self.pane_snapshots = captured;
            }
            self.apply_focus_layout(pane_id);
            if let Some(window) = self.window.as_ref() {
                window.invalidate();
            }
        }
        self.pane_hold_capture = None;
        log::debug!("paint_impl before call_draw elapsed={:?}", start.elapsed());

        let seg = Instant::now();
        self.call_draw(frame).ok();
        self.log_frame_segment("draw", seg);
        self.last_frame_duration = start.elapsed();
        if self.last_frame_duration > Duration::from_millis(500) {
            log::error!(
                "SLOW FRAME: paint_impl took {:?} (fps={})",
                self.last_frame_duration,
                self.fps
            );
        }
        log::debug!(
            "paint_impl elapsed={:?}, fps={}",
            self.last_frame_duration,
            self.fps
        );
        metrics::histogram!("gui.paint.impl").record(self.last_frame_duration);
        metrics::histogram!("gui.paint.impl.rate").record(1.);

        // If self.has_animation is some, then the last render detected
        // image attachments with multiple frames, so we also need to
        // invalidate the viewport when the next frame is due
        let next_due = *self.has_animation.borrow();
        // A frame is due within one display frame: an animation is running.
        let frame_interval = Duration::from_secs_f64(1.0 / self.config.max_fps.clamp(1, 240) as f64);
        let animating = next_due.map_or(false, |due| due <= Instant::now() + frame_interval);
        self.log_animation_frame(start, animating);
        if !animating {
            self.prewarm_scaled_font_step();
        }
        if animating {
            // A frame slower than one refresh must not eat animation time,
            // or the next frame jumps ahead by the stall.
            if let Some(overrun) = self.last_frame_duration.checked_sub(frame_interval) {
                self.pane_anims.borrow_mut().delay(overrun);
            }
        }

        if self.focused.is_some() && animating && self.config.animation_vsync {
            if let Some(window) = self.window.as_ref() {
                if window.paces_repaints_to_display() {
                    // The frame callback requested by this paint is still
                    // pending, so the invalidate is held until the compositor
                    // wants the next frame: one paint per display refresh.
                    // Deferred so it never runs inside this paint.
                    self.scheduled_animation.borrow_mut().take();
                    let window = window.clone();
                    promise::spawn::spawn(async move {
                        window.invalidate();
                    })
                    .detach();
                    return;
                }
            }
        }

        if self.focused.is_some() {
            if let Some(next_due) = next_due {
                let prior = self.scheduled_animation.borrow_mut().take();
                match prior {
                    Some(prior) if prior <= next_due => {
                        // Already due before that time
                    }
                    _ => {
                        self.scheduled_animation.borrow_mut().replace(next_due);
                        let window = self.window.clone().take().unwrap();
                        promise::spawn::spawn(async move {
                            Timer::at(next_due).await;
                            let win = window.clone();
                            window.notify(TermWindowNotif::Apply(Box::new(move |tw| {
                                tw.scheduled_animation.borrow_mut().take();
                                win.invalidate();
                            })));
                        })
                        .detach();
                    }
                }
            }
        }
    }

    /// With `log_animation_frames`, log one part of a frame that took longer
    /// than 3 ms, to find what makes a slow frame slow.
    pub(crate) fn log_frame_segment(&self, what: &str, since: Instant) {
        if !self.config.log_animation_frames {
            return;
        }
        let ms = since.elapsed().as_secs_f64() * 1000.0;
        if ms >= 3.0 {
            log::info!(target: "anim_frames", "  segment {what}: {ms:.2}ms");
        }
    }

    fn log_animation_frame(&self, start: Instant, animating: bool) {
        let mut last = self.last_anim_frame.borrow_mut();
        if !self.config.log_animation_frames {
            *last = None;
            return;
        }
        let gap = last.map(|prev| start.duration_since(prev).as_secs_f64() * 1000.0);
        if animating || gap.is_some() {
            log::info!(
                target: "anim_frames",
                "paint={:.2}ms gap={} vsync={}",
                self.last_frame_duration.as_secs_f64() * 1000.0,
                gap.map_or("-".to_string(), |g| format!("{g:.2}ms")),
                self.config.animation_vsync,
            );
        }
        *last = if animating { Some(start) } else { None };
    }

    pub fn paint_modal(&mut self) -> anyhow::Result<()> {
        if let Some(modal) = self.get_modal() {
            for computed in modal.computed_element(self)?.iter() {
                let mut ui_items = computed.ui_items();

                let gl_state = self.render_state.as_ref().unwrap();
                self.render_element(&computed, gl_state, None)?;

                self.ui_items.append(&mut ui_items);
            }
        }

        Ok(())
    }

    pub fn paint_pass(&mut self) -> anyhow::Result<()> {
        {
            let gl_state = self.render_state.as_ref().unwrap();
            for layer in gl_state.layers.borrow().iter() {
                layer.clear_quad_allocation();
            }
        }

        // Clear out UI item positions; we'll rebuild these as we render
        self.ui_items.clear();

        let seg = Instant::now();
        let panes = self.get_panes_to_render();
        self.log_frame_segment("get_panes", seg);
        let seg = Instant::now();
        self.refit_auto_panes(&panes);
        self.log_frame_segment("refit", seg);
        let focused = self.focused.is_some();
        let window_is_transparent =
            !self.window_background.is_empty() || crate::termwindow::effective_window_opacity(self.config.window_background_opacity) != 1.0;

        let start = Instant::now();
        let gl_state = self.render_state.as_ref().unwrap();
        let layer = gl_state
            .layer_for_zindex(0)
            .context("layer_for_zindex(0)")?;
        let mut layers = layer.quad_allocator();
        log::trace!("quad map elapsed {:?}", start.elapsed());
        metrics::histogram!("quad.map").record(start.elapsed());

        let mut paint_terminal_background = false;

        // Render the full window background
        match (self.window_background.is_empty(), self.allow_images) {
            (false, AllowImage::Yes | AllowImage::Scale(_)) => {
                let bg_color = self.palette().background.to_linear();

                let top = panes
                    .iter()
                    .find(|p| p.is_active)
                    .map(|p| match self.get_viewport(p.pane.pane_id()) {
                        Some(top) => top,
                        None => p.pane.get_dimensions().physical_top,
                    })
                    .unwrap_or(0);

                let loaded_any = self
                    .render_backgrounds(bg_color, top)
                    .context("render_backgrounds")?;

                if !loaded_any {
                    // Either there was a problem loading the background(s)
                    // or they haven't finished loading yet.
                    // Use the regular terminal background until that changes.
                    paint_terminal_background = true;
                }
            }
            _ if window_is_transparent => {
                // Avoid doubling up the background color: the panes
                // will render out through the padding so there
                // should be no gaps that need filling in
            }
            _ => {
                paint_terminal_background = true;
            }
        }

        if paint_terminal_background {
            // Regular window background color
            let background = if panes.len() == 1 {
                // If we're the only pane, use the pane's palette
                // to draw the padding background
                panes[0].pane.palette().background
            } else {
                self.palette().background
            }
            .to_linear()
            .mul_alpha(crate::termwindow::effective_window_opacity(self.config.window_background_opacity));

            self.filled_rectangle(
                &mut layers,
                0,
                euclid::rect(
                    0.,
                    0.,
                    self.dimensions.pixel_width as f32,
                    self.dimensions.pixel_height as f32,
                ),
                background,
            )
            .context("filled_rectangle for window background")?;
        }

        // Spot panes that moved, resized, appeared or vanished since the
        // last frame and animate the change.
        let anim_duration = self.pane_animation_duration();
        let frame_now = Instant::now();
        let cell_size = (
            self.render_metrics.cell_size.width as f32,
            self.render_metrics.cell_size.height as f32,
        );
        let snaps: Vec<(mux::pane::PaneId, crate::pane_anim::PaneSnap)> = panes
            .iter()
            .map(|pos| {
                let (x, y, w, h) = self.pane_content_rect(pos);
                let bg = pos
                    .pane
                    .palette()
                    .background
                    .to_linear()
                    .mul_alpha(crate::termwindow::effective_window_opacity(
                        self.config.window_background_opacity,
                    ));
                (
                    pos.pane.pane_id(),
                    crate::pane_anim::PaneSnap {
                        rect: crate::pane_anim::PxRect { x, y, w, h },
                        bounds: [
                            pos.left == 0,
                            pos.top == 0,
                            pos.left + pos.width >= self.terminal_size.cols as usize,
                            pos.top + pos.height >= self.terminal_size.rows as usize,
                        ],
                        bg: [bg.0, bg.1, bg.2, bg.3],
                    },
                )
            })
            .collect();
        if let Some(duration) = anim_duration {
            let env = [
                self.dimensions.pixel_width as f32,
                self.dimensions.pixel_height as f32,
                cell_size.0,
                cell_size.1,
                self.sidebar_x_offset(),
            ];
            let split_drag = matches!(
                &self.dragging,
                Some((item, _)) if matches!(item.item_type, crate::termwindow::UIItemType::Split(_))
            );
            if split_drag {
                self.pane_anims.borrow_mut().settle(&snaps, env);
            } else {
                self.pane_anims
                    .borrow_mut()
                    .observe_frame(&snaps, env, cell_size, frame_now, duration);
            }

            // A closed pane's last rect fades out beneath the survivors.
            let ghosts = self.pane_anims.borrow().ghosts(frame_now, duration);
            for (rect, bg, opacity) in ghosts {
                let pad = (cell_size.0 / 2.0, cell_size.1 / 2.0);
                self.filled_rectangle(
                    &mut layers,
                    0,
                    euclid::rect(rect.x - pad.0, rect.y - pad.1, rect.w + 2.0 * pad.0, rect.h + 2.0 * pad.1),
                    window::color::LinearRgba(bg[0], bg[1], bg[2], bg[3] * opacity),
                )
                .context("closed pane ghost")?;
            }
        }

        let any_lift = anim_duration.is_some()
            && snaps.iter().any(|(id, _)| self.pane_anims.borrow().on_top(*id));
        let lifted_layer = if any_lift {
            Some(
                self.render_state
                    .as_ref()
                    .unwrap()
                    .layer_for_zindex(1)
                    .context("layer_for_zindex(1)")?,
            )
        } else {
            None
        };
        let mut lifted = lifted_layer.as_ref().map(|l| l.quad_allocator());

        // Pane holds (pane_hold.rs): capture every pane's picture on the
        // frame before a released divider move is applied, and draw held
        // pictures until their panes have redrawn.
        let capturing = self.pending_split_resize.is_some() || self.pending_focus_layout.is_some();
        self.pane_hold_capture = capturing.then(HashMap::new);
        {
            let env = [
                self.dimensions.pixel_width as f32,
                self.dimensions.pixel_height as f32,
                cell_size.0,
                cell_size.1,
                self.sidebar_x_offset(),
            ];
            if self.pane_hold_env != Some(env) {
                self.pane_holds.clear();
                self.pane_hold_quads.clear();
                self.pane_hold_env = Some(env);
            }
            let max = Duration::from_millis(self.config.pane_resize_hold_ms);
            for (id, why, held) in self.pane_holds.release_due(frame_now, max) {
                self.pane_hold_quads.remove(&id);
                if self.config.log_animation_frames {
                    log::info!(
                        target: "anim_frames",
                        "  pane {} hold released after {:.0}ms ({:?})",
                        id,
                        held.as_secs_f64() * 1000.0,
                        why
                    );
                }
            }
            for id in self.pane_holds.panes() {
                if !snaps.iter().any(|(sid, _)| *sid == id) {
                    self.pane_holds.remove(id);
                    self.pane_hold_quads.remove(&id);
                }
            }
        }

        for (pos, (_, snap)) in panes.into_iter().zip(snaps.iter()) {
            let id = pos.pane.pane_id();
            if capturing {
                if pos.is_active {
                    self.update_text_cursor(&pos);
                }
                let mut quads = HeapQuadAllocator::default();
                self.paint_pane(&pos, &mut TripleLayerQuadAllocator::Heap(&mut quads))
                    .context("paint_pane for hold")?;
                quads.apply_to(&mut layers).context("apply held pane")?;
                let bg = self.pane_background_rect(&pos)?;
                let bg = crate::pane_anim::PxRect { x: bg.min_x(), y: bg.min_y(), w: bg.width(), h: bg.height() };
                if let Some(capture) = self.pane_hold_capture.as_mut() {
                    capture.insert(id, (quads, bg));
                }
                continue;
            }
            if let Some(from) = self.pane_holds.rect(id) {
                // Holds are kept in background rects: the cells plus the
                // divider gutters and window margins the pane also paints.
                let bg = self.pane_background_rect(&pos)?;
                let bg = crate::pane_anim::PxRect { x: bg.min_x(), y: bg.min_y(), w: bg.width(), h: bg.height() };
                let moved = (from.x - bg.x).abs() >= 1.0
                    || (from.y - bg.y).abs() >= 1.0
                    || (from.w - bg.w).abs() >= 1.0
                    || (from.h - bg.h).abs() >= 1.0;
                if !moved {
                    // The release did not touch this pane.
                    self.pane_holds.remove(id);
                    self.pane_hold_quads.remove(&id);
                } else if let Some(quads) = self.pane_hold_quads.get(&id) {
                    // Follow a running reveal tween, so the picture grows
                    // into its new place with the animation.
                    // The tween runs on the content rect; widen it by the
                    // same margins the final background has around it.
                    let content = anim_duration
                        .and_then(|d| self.pane_anims.borrow().current(id, frame_now, d))
                        .unwrap_or(snap.rect);
                    let (ml, mt) = (snap.rect.x - bg.x, snap.rect.y - bg.y);
                    let (mr, mb) = (
                        bg.x + bg.w - (snap.rect.x + snap.rect.w),
                        bg.y + bg.h - (snap.rect.y + snap.rect.h),
                    );
                    let target = crate::pane_anim::PxRect {
                        x: content.x - ml,
                        y: content.y - mt,
                        w: content.w + ml + mr,
                        h: content.h + mt + mb,
                    };
                    let bg = snap.bg;
                    self.filled_rectangle(
                        &mut layers,
                        0,
                        euclid::rect(target.x, target.y, target.w, target.h),
                        window::color::LinearRgba(bg[0], bg[1], bg[2], bg[3]),
                    )
                    .context("held pane background")?;
                    let mark = layers.mark();
                    quads.apply_to(&mut layers).context("apply held pane")?;
                    let tr = crate::pane_hold::fit_into(&from, &target).to_vertex_space(
                        self.dimensions.pixel_width as f32 / 2.0,
                        self.dimensions.pixel_height as f32 / 2.0,
                    );
                    layers.transform_since(mark, &tr);
                    continue;
                }
            }
            if pos.is_active {
                self.update_text_cursor(&pos);
                if focused {
                    pos.pane.advise_focus();
                    mux::Mux::get().record_focus_for_current_identity(pos.pane.pane_id());
                }
            }
            let transform = match anim_duration {
                Some(duration) => self.pane_anims.borrow().transform(
                    pos.pane.pane_id(),
                    &snap.rect,
                    frame_now,
                    duration,
                    (cell_size.0 / 2.0, cell_size.1 / 2.0),
                ),
                None => None,
            };
            // Quads are positioned relative to the window centre.
            let transform = transform.map(|tr| {
                tr.to_vertex_space(
                    self.dimensions.pixel_width as f32 / 2.0,
                    self.dimensions.pixel_height as f32 / 2.0,
                )
            });
            // The shrinking half of a swap is drawn from its picture before
            // the swap, shrunk into the rect it shows now: its new layout is
            // the small one, and magnifying that enlarges its text.
            if let Some(duration) = anim_duration {
                let shrinking = self.pane_anims.borrow().shrinking_swap(id);
                let current = self.pane_anims.borrow().current(id, frame_now, duration);
                if let (true, Some(content), Some((quads, from))) =
                    (shrinking, current, self.pane_snapshots.get(&id))
                {
                    let bg = self.pane_background_rect(&pos)?;
                    let bg = crate::pane_anim::PxRect { x: bg.min_x(), y: bg.min_y(), w: bg.width(), h: bg.height() };
                    let (ml, mt) = (snap.rect.x - bg.x, snap.rect.y - bg.y);
                    let (mr, mb) = (
                        bg.x + bg.w - (snap.rect.x + snap.rect.w),
                        bg.y + bg.h - (snap.rect.y + snap.rect.h),
                    );
                    let target = crate::pane_anim::PxRect {
                        x: content.x - ml,
                        y: content.y - mt,
                        w: content.w + ml + mr,
                        h: content.h + mt + mb,
                    };
                    let mark = layers.mark();
                    quads.apply_to(&mut layers).context("apply swap snapshot")?;
                    let tr = crate::pane_hold::fit_into(from, &target).to_vertex_space(
                        self.dimensions.pixel_width as f32 / 2.0,
                        self.dimensions.pixel_height as f32 / 2.0,
                    );
                    layers.transform_since(mark, &tr);
                    continue;
                }
            }
            let on_top = anim_duration.is_some() && self.pane_anims.borrow().on_top(pos.pane.pane_id());
            if let (true, Some(tr)) = (on_top, transform) {
                // The growing half of a swap is painted on the layer above
                // the others, so it covers the pane shrinking out of its way.
                let lifted_layers = lifted.as_mut().context("lift layer")?;
                let mark = lifted_layers.mark();
                let seg = Instant::now();
                self.pane_line_stats.set((0, 0, Duration::ZERO));
                self.paint_pane(&pos, lifted_layers).context("paint_pane")?;
                {
                    let (hits, misses, miss_time) = self.pane_line_stats.get();
                    self.log_frame_segment(
                        &format!(
                            "pane {} (lines: {} cached, {} rebuilt in {:.2}ms)",
                            pos.pane.pane_id(),
                            hits,
                            misses,
                            miss_time.as_secs_f64() * 1000.0
                        ),
                        seg,
                    );
                }
                lifted_layers.transform_since(mark, &tr);
                continue;
            }
            let mark = layers.mark();
            let seg = Instant::now();
                self.pane_line_stats.set((0, 0, Duration::ZERO));
            self.paint_pane(&pos, &mut layers).context("paint_pane")?;
            {
                    let (hits, misses, miss_time) = self.pane_line_stats.get();
                    self.log_frame_segment(
                        &format!(
                            "pane {} (lines: {} cached, {} rebuilt in {:.2}ms)",
                            pos.pane.pane_id(),
                            hits,
                            misses,
                            miss_time.as_secs_f64() * 1000.0
                        ),
                        seg,
                    );
                }
            if let Some(tr) = transform {
                layers.transform_since(mark, &tr);
            }
        }
        drop(lifted);
        {
            let anims = self.pane_anims.borrow();
            self.pane_snapshots.retain(|id, _| anims.shrinking_swap(*id));
        }

        let sliding = match anim_duration {
            Some(duration) => {
                let more = self.pane_anims.borrow_mut().prune(frame_now, duration)
                    || !self.pane_holds.is_empty();
                if more {
                    let frame = std::time::Duration::from_millis(
                        1000 / self.config.max_fps.clamp(1, 240),
                    );
                    self.update_next_frame_time(Some(frame_now + frame));
                }
                self.pane_anims.borrow().has_tweens()
            }
            None => {
                // A held pane still needs frames to notice its release.
                if !self.pane_holds.is_empty() {
                    let frame = std::time::Duration::from_millis(
                        1000 / self.config.max_fps.clamp(1, 240),
                    );
                    self.update_next_frame_time(Some(frame_now + frame));
                }
                false
            }
        };

        // Splits are drawn at final positions; hide them while panes slide.
        if !sliding {
            if let Some(pane) = self.get_active_pane_or_overlay() {
                let splits = self.get_splits();
                for split in &splits {
                    self.paint_split(&mut layers, split, &pane)
                        .context("paint_split")?;
                }
            }
        }

        if self.tab_drag.is_some() {
            self.paint_drop_zone_overlay(&mut layers)
                .context("paint_drop_zone_overlay")?;
        }

        if self.pane_drag.is_some() {
            self.paint_pane_drag_overlay(&mut layers)
                .context("paint_pane_drag_overlay")?;
        }

        if self.pane_long_press.as_ref().map_or(false, |lp| lp.revealed) {
            self.paint_pane_remove_overlay(&mut layers)
                .context("paint_pane_remove_overlay")?;
        }

        // Called unconditionally: it returns early when there is nothing to
        // draw, and that early return is what clears the stored toast rect.
        // Gating the call here would leave a stale rect behind instead.
        self.paint_toast_toolbar(&mut layers)
            .context("paint_toast_toolbar")?;
        // Same: returns early unless the hovered pane (or the pane in hand)
        // has a grip to show.
        self.paint_pane_grip(&mut layers)
            .context("paint_pane_grip")?;

        // Aggregate Claude-agent waiting count for the tab-bar badge. Runs
        // regardless of sidebar visibility (the badge lives on the tab bar),
        // and is throttled internally to once per second.
        let seg = Instant::now();
        self.update_agents_waiting();
        self.log_frame_segment("agents_waiting", seg);

        if self.show_tab_sidebar {
            let seg = Instant::now();
            self.paint_tab_sidebar(&mut layers)
                .context("paint_tab_sidebar")?;
            self.log_frame_segment("sidebar", seg);
        }

        if self.show_tab_bar {
            let seg = Instant::now();
            self.paint_tab_bar(&mut layers).context("paint_tab_bar")?;
            self.log_frame_segment("tab_bar", seg);
        }

        let seg = Instant::now();
        self.paint_brief_bar(&mut layers)
            .context("paint_brief_bar")?;
        self.log_frame_segment("brief_bar", seg);

        self.paint_window_borders(&mut layers)
            .context("paint_window_borders")?;
        drop(layers);
        self.paint_modal().context("paint_modal")?;

        Ok(())
    }
}

use crate::tabbar::TabBarItem;
use crate::termwindow::{
    DropZone, GuiWin, MouseCapture, PositionedSplit, ScrollHit, TabDragState, TabSidebarItem,
    TermWindowNotif, UIItem, UIItemType, TMB,
};
use ::window::{
    Modifiers, MouseButtons as WMB, MouseCursor, MouseEvent, MouseEventKind as WMEK, MousePress,
    WindowDecorations, WindowOps, WindowState,
};
use config::keyassignment::{KeyAssignment, MouseEventTrigger, SpawnTabDomain};
use config::MouseEventAltScreen;
use mux::pane::{Pane, WithPaneLines};
use mux::tab::{SplitDirection, SplitRequest, SplitSize, TabId};
use mux::Mux;
use std::convert::TryInto;
use std::ops::Sub;
use std::sync::Arc;
use std::time::Duration;
use termwiz::hyperlink::Hyperlink;
use termwiz::surface::Line;
use terminaler_dynamic::ToDynamic;
use terminaler_term::input::{MouseButton, MouseEventKind as TMEK};
use terminaler_term::{ClickPosition, LastMouseClick, StableRowIndex};

impl super::TermWindow {
    fn resolve_ui_item(&self, event: &MouseEvent) -> Option<UIItem> {
        let x = event.coords.x;
        let y = event.coords.y;
        self.ui_items
            .iter()
            .rev()
            .find(|item| item.hit_test(x, y))
            .cloned()
    }

    fn leave_ui_item(&mut self, item: &UIItem) {
        match item.item_type {
            UIItemType::TabBar(_) => {
                self.update_title_post_status();
            }
            UIItemType::CloseTab(_)
            | UIItemType::AboveScrollThumb
            | UIItemType::BelowScrollThumb
            | UIItemType::ScrollThumb
            | UIItemType::Split(_)
            | UIItemType::ProfileDropdownItem(_)
            | UIItemType::TabSidebar(_) => {}
        }
    }

    fn enter_ui_item(&mut self, item: &UIItem) {
        match item.item_type {
            UIItemType::TabBar(_) => {}
            UIItemType::CloseTab(_)
            | UIItemType::AboveScrollThumb
            | UIItemType::BelowScrollThumb
            | UIItemType::ScrollThumb
            | UIItemType::Split(_)
            | UIItemType::ProfileDropdownItem(_)
            | UIItemType::TabSidebar(_) => {}
        }
    }

    pub fn mouse_event_impl(&mut self, event: MouseEvent, context: &dyn WindowOps) {
        log::trace!("{:?}", event);
        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };

        self.current_mouse_event.replace(event.clone());

        let border = self.get_os_border();

        // Compute top_pixel_y as f32, exactly matching the rendering path
        // (render/pane.rs). Previously each component was truncated to isize
        // separately, which could shift the cell grid by ~1px when padding_top
        // had a fractional part (e.g. Cells(0.5) with odd cell height).
        let tab_bar_height_f32 = if self.show_tab_bar && !self.config.tab_bar_at_bottom {
            self.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };
        let (padding_left, padding_top) = self.padding_left_top();
        let top_pixel_y = tab_bar_height_f32 + padding_top + border.top.get() as f32;

        // Use pane-specific scaled cell size when the pane has a custom font
        // scale (e.g. from Ctrl+scroll zoom). The renderer uses scaled metrics,
        // so mouse→cell conversion must match.
        let pane_scale = self.pane_font_scale(pane.pane_id());
        let cell_size = if (pane_scale - 1.0).abs() > 0.001 {
            let key = Self::font_scale_key(pane_scale);
            self.scaled_font_configs
                .get(&key)
                .map(|(_, metrics)| metrics.cell_size)
                .unwrap_or(self.render_metrics.cell_size)
        } else {
            self.render_metrics.cell_size
        };

        let sidebar_offset = self.sidebar_x_offset();
        let left_pixel_x = padding_left + border.left.get() as f32 + sidebar_offset;

        // Convert pixel coords to cell coords using the same top_pixel_y and
        // left_pixel_x that the renderer uses, ensuring exact alignment.
        let y_pixel_in_content = (event.coords.y as f32 - top_pixel_y).max(0.);
        let y = (y_pixel_in_content as isize / cell_size.height) as i64;

        let x_pixel_in_content = (event.coords.x as f32 - left_pixel_x).max(0.);
        let x = x_pixel_in_content / cell_size.width as f32;
        let x = if !pane.is_mouse_grabbed() {
            // Round the x coordinate so that we're a bit more forgiving of
            // the horizontal position when selecting cells
            x.round()
        } else {
            x
        }
        .trunc() as usize;

        let mut y_pixel_offset = (event.coords.y as f32 - top_pixel_y) as isize;
        if y > 0 {
            y_pixel_offset = y_pixel_offset.max(0) % cell_size.height;
        }

        let mut x_pixel_offset = (event.coords.x as f32 - left_pixel_x) as isize;
        if x > 0 {
            x_pixel_offset = x_pixel_offset.max(0) % cell_size.width;
        }

        self.last_mouse_coords = (x, y);

        let mut capture_mouse = false;

        match event.kind {
            WMEK::Release(ref press) => {
                self.current_mouse_capture = None;
                self.current_mouse_buttons.retain(|p| p != press);
                if press == &MousePress::Left {
                    if let Some(tab_drag) = self.tab_drag.take() {
                        if tab_drag.threshold_exceeded
                            && tab_drag.target_pane.is_some()
                            && tab_drag.target_zone.is_some()
                        {
                            self.execute_tab_drag_drop(tab_drag);
                        }
                        context.invalidate();
                        return;
                    }
                }
                if press == &MousePress::Left && self.window_drag_position.take().is_some() {
                    // Completed a window drag
                    return;
                }
                if press == &MousePress::Left {
                    if let Some((ref item, _)) = self.dragging {
                        if matches!(item.item_type, UIItemType::TabSidebar(TabSidebarItem::ResizeHandle)) {
                            self.dragging.take();
                            self.finish_sidebar_resize();
                            return;
                        }
                    }
                    if self.dragging.take().is_some() {
                        // Completed a drag
                        return;
                    }
                }
            }

            WMEK::Press(ref press) => {
                capture_mouse = true;

                // Perform click counting
                let button = mouse_press_to_tmb(press);

                let click_position = ClickPosition {
                    column: x,
                    row: y,
                    x_pixel_offset,
                    y_pixel_offset,
                };

                let click = match self.last_mouse_click.take() {
                    None => LastMouseClick::new(button, click_position),
                    Some(click) => click.add(button, click_position),
                };
                self.last_mouse_click = Some(click);
                self.current_mouse_buttons.retain(|p| p != press);
                self.current_mouse_buttons.push(*press);
            }

            WMEK::Move => {
                // Tab drag-to-split: check threshold and track target
                if let Some(ref tab_drag) = self.tab_drag {
                    if !tab_drag.threshold_exceeded {
                        let dx = (event.coords.x - tab_drag.start_coords.0) as f64;
                        let dy = (event.coords.y - tab_drag.start_coords.1) as f64;
                        if (dx * dx + dy * dy).sqrt() >= 5.0 {
                            // Compute the tab index to switch to before
                            // releasing the borrow on self.tab_drag
                            let switch_to = {
                                let mux = Mux::get();
                                let win = mux.get_window(self.mux_window_id);
                                win.and_then(|w| w.idx_by_id(tab_drag.dest_tab_id))
                            };
                            // Now mutate
                            self.tab_drag.as_mut().unwrap().threshold_exceeded = true;
                            if let Some(idx) = switch_to {
                                self.activate_tab(idx as isize).ok();
                            }
                        }
                    }
                    // Re-check after potential mutation
                }
                if self.tab_drag.as_ref().map_or(false, |td| td.threshold_exceeded) {
                    self.update_tab_drag_target(&event);
                    context.set_cursor(Some(MouseCursor::Arrow));
                    context.invalidate();
                    return;
                }

                if let Some(start) = self.window_drag_position.as_ref() {
                    // Dragging the window
                    // Compute the distance since the initial event
                    let delta_x = start.screen_coords.x - event.screen_coords.x;
                    let delta_y = start.screen_coords.y - event.screen_coords.y;

                    // Now compute a new window position.
                    // We don't have a direct way to get the position,
                    // but we can infer it by comparing the mouse coords
                    // with the screen coords in the initial event.
                    // This computes the original top_left position,
                    // and applies the total drag delta to it.
                    let top_left = ::window::ScreenPoint::new(
                        (start.screen_coords.x - start.coords.x) - delta_x,
                        (start.screen_coords.y - start.coords.y) - delta_y,
                    );
                    // and now tell the window to go there
                    context.set_window_position(top_left);
                    return;
                }

                if let Some((item, start_event)) = self.dragging.take() {
                    self.drag_ui_item(item, start_event, x, y, event, context);
                    return;
                }

                // Update hovered pane for toast toolbar
                let new_hovered = self.pane_id_at_pixel_coords(event.coords.x, event.coords.y);
                if new_hovered != self.hovered_pane_id {
                    self.hovered_pane_id = new_hovered;
                    self.toast_expanded_for = None;
                    context.invalidate();
                }

                // Toast expand/collapse logic
                if let Some((pane_id, btn)) = self.toast_button_at(&event) {
                    if btn == "trigger" && self.toast_expanded_for != Some(pane_id) {
                        self.toast_expanded_for = Some(pane_id);
                        context.invalidate();
                    }
                    context.set_cursor(Some(MouseCursor::Hand));
                    return;
                } else if let Some(expanded_id) = self.toast_expanded_for {
                    // Collapse if cursor is outside expanded toast + margin
                    use crate::termwindow::render::pane::{
                        TOAST_COLLAPSE_MARGIN, TOAST_HEIGHT, TOAST_WIDTH,
                    };
                    let collapsed = 'collapse: {
                        let panes = self.get_panes_to_render();
                        let pos = match panes.iter().find(|p| p.pane.pane_id() == expanded_id) {
                            Some(p) => p,
                            None => break 'collapse true,
                        };
                        let cell_w = self.render_metrics.cell_size.width as f32;
                        let cell_h = self.render_metrics.cell_size.height as f32;
                        let (pad_l, pad_t) = self.padding_left_top();
                        let tab_h = if self.show_tab_bar {
                            self.tab_bar_pixel_height().unwrap_or(0.)
                        } else { 0. };
                        let top_h = if self.config.tab_bar_at_bottom { 0.0 } else { tab_h };
                        let bdr = self.get_os_border();
                        let top_py = top_h + pad_t + bdr.top.get() as f32;
                        let t_bg_y = if pos.top == 0 {
                            top_py - pad_t
                        } else {
                            top_py + (pos.top as f32 * cell_h) - (cell_h / 2.0)
                        };
                        let t_bg_right = if pos.left + pos.width >= self.terminal_size.cols as usize {
                            self.effective_right_edge()
                        } else {
                            let (bx, wd) = if pos.left == 0 {
                                (0.0f32, pad_l + bdr.left.get() as f32 + (cell_w / 2.0))
                            } else {
                                (pad_l + bdr.left.get() as f32 - (cell_w / 2.0)
                                    + (pos.left as f32 * cell_w), cell_w)
                            };
                            bx + (pos.width as f32 * cell_w) + wd
                        };
                        let tl = t_bg_right - TOAST_WIDTH;
                        let tt = t_bg_y;
                        let mx = event.coords.x as f32;
                        let my = event.coords.y as f32;
                        mx < tl - TOAST_COLLAPSE_MARGIN
                            || mx > t_bg_right + TOAST_COLLAPSE_MARGIN
                            || my < tt - TOAST_COLLAPSE_MARGIN
                            || my > tt + TOAST_HEIGHT + TOAST_COLLAPSE_MARGIN
                    };
                    if collapsed {
                        self.toast_expanded_for = None;
                        context.invalidate();
                    }
                }
            }
            _ => {}
        }

        // Toast toolbar: intercept clicks on toast buttons
        if matches!(event.kind, WMEK::Press(MousePress::Left)) {
            if let Some((pane_id, btn)) = self.toast_button_at(&event) {
                if btn == "trigger" {
                    self.toast_expanded_for = Some(pane_id);
                    context.invalidate();
                    return;
                } else if btn == "close" {
                    Mux::get().remove_pane(pane_id);
                } else if btn == "move-to-tab" {
                    self.execute_pane_to_tab(pane_id);
                } else {
                    self.apply_snap_layout_to_pane(pane_id, btn);
                }
                self.toast_expanded_for = None;
                self.hovered_pane_id = None;
                context.invalidate();
                return;
            }
        }

        // Long press overlay: intercept clicks when overlay is showing
        if matches!(event.kind, WMEK::Press(MousePress::Left)) {
            if self.pane_long_press.as_ref().map_or(false, |lp| lp.revealed) {
                let pane_id = self.pane_long_press.as_ref().unwrap().pane_id;
                if let Some(btn) = self.overlay_button_at(&event, pane_id) {
                    self.pane_long_press = None;
                    if btn == "close" {
                        Mux::get().remove_pane(pane_id);
                    } else if btn == "move-to-tab" {
                        self.execute_pane_to_tab(pane_id);
                    } else {
                        self.apply_snap_layout_to_pane(pane_id, btn);
                    }
                } else {
                    self.pane_long_press = None;
                }
                context.invalidate();
                return;
            }
        }

        // Ctrl+scroll: adjust per-pane font size instead of scrolling
        if let WMEK::VertWheel(amount) = event.kind {
            if event.modifiers.contains(Modifiers::CTRL) {
                let pane_id = pane.pane_id();
                let factor = if amount > 0 { 1.1 } else { 1.0 / 1.1 };
                let current = self.pane_font_scale(pane_id);
                let new_scale = (current * factor).clamp(0.5, 3.0);
                {
                    let mut states = self.pane_state.borrow_mut();
                    let state = states.entry(pane_id).or_insert_with(Default::default);
                    state.font_scale = new_scale;
                }
                self.resize_pane_for_font_scale(pane_id);
                self.shape_generation += 1;
                self.shape_cache.borrow_mut().clear();
                context.invalidate();
                return;
            }
        }

        let prior_ui_item = self.last_ui_item.clone();

        let ui_item = if matches!(self.current_mouse_capture, None | Some(MouseCapture::UI)) {
            let ui_item = self.resolve_ui_item(&event);

            match (self.last_ui_item.take(), &ui_item) {
                (Some(prior), Some(item)) => {
                    if prior != *item || !self.config.use_fancy_tab_bar {
                        self.leave_ui_item(&prior);
                        self.enter_ui_item(item);
                        context.invalidate();
                    }
                }
                (Some(prior), None) => {
                    self.leave_ui_item(&prior);
                    context.invalidate();
                }
                (None, Some(item)) => {
                    self.enter_ui_item(item);
                    context.invalidate();
                }
                (None, None) => {}
            }

            ui_item
        } else {
            None
        };

        // Dismiss profile dropdown on any click outside dropdown items
        if matches!(event.kind, WMEK::Press(_)) {
            if let Some(modal) = self.get_modal() {
                if modal
                    .downcast_ref::<crate::termwindow::profile_dropdown::ProfileDropdown>()
                    .is_some()
                {
                    let is_dropdown_item = ui_item.as_ref().map_or(false, |item| {
                        matches!(item.item_type, UIItemType::ProfileDropdownItem(_))
                    });
                    if !is_dropdown_item {
                        self.cancel_modal();
                        context.invalidate();
                        // Don't return — let the click propagate to whatever was under it
                    }
                }
            }
        }

        if let Some(item) = ui_item.clone() {
            if capture_mouse {
                self.current_mouse_capture = Some(MouseCapture::UI);
            }
            self.mouse_event_ui_item(item, pane, y, event, context);
        } else if matches!(
            self.current_mouse_capture,
            None | Some(MouseCapture::TerminalPane(_))
        ) {
            self.mouse_event_terminal(
                pane,
                ClickPosition {
                    column: x,
                    row: y,
                    x_pixel_offset,
                    y_pixel_offset,
                },
                event.clone(),
                context,
                capture_mouse,
            );

            // Ctrl+Right-click in terminal area: show pane layout overlay
            if matches!(event.kind, WMEK::Press(MousePress::Right))
                && event.modifiers.contains(Modifiers::CTRL)
            {
                self.start_pane_overlay(&event);
            }

        }

        if prior_ui_item != ui_item {
            self.update_title_post_status();
        }
    }

    pub fn mouse_leave_impl(&mut self, context: &dyn WindowOps) {
        self.current_mouse_event = None;
        self.hovered_pane_id = None;
        self.toast_expanded_for = None;
        self.update_title();
        context.set_cursor(Some(MouseCursor::Arrow));
        context.invalidate();
    }

    fn drag_split(
        &mut self,
        mut item: UIItem,
        split: PositionedSplit,
        start_event: MouseEvent,
        x: usize,
        y: i64,
        context: &dyn WindowOps,
    ) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };
        let delta = match split.direction {
            SplitDirection::Horizontal => (x as isize).saturating_sub(split.left as isize),
            SplitDirection::Vertical => (y as isize).saturating_sub(split.top as isize),
        };

        if delta != 0 {
            tab.resize_split_by(split.index, delta);
            if let Some(split) = tab.iter_splits().into_iter().nth(split.index) {
                item.item_type = UIItemType::Split(split);
                context.invalidate();
            }
        }
        self.dragging.replace((item, start_event));
    }

    fn drag_scroll_thumb(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };

        let dims = pane.get_dimensions();
        let current_viewport = self.get_viewport(pane.pane_id());

        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };
        let (top_bar_height, bottom_bar_height) = if self.config.tab_bar_at_bottom {
            (0.0, tab_bar_height)
        } else {
            (tab_bar_height, 0.0)
        };

        let border = self.get_os_border();
        let y_offset = top_bar_height + border.top.get() as f32;

        let from_top = start_event.coords.y.saturating_sub(item.y as isize);
        let effective_thumb_top = event
            .coords
            .y
            .saturating_sub(y_offset as isize + from_top)
            .max(0) as usize;

        // Convert thumb top into a row index by reversing the math
        // in ScrollHit::thumb
        let row = ScrollHit::thumb_top_to_scroll_top(
            effective_thumb_top,
            &*pane,
            current_viewport,
            self.dimensions.pixel_height.saturating_sub(
                y_offset as usize + border.bottom.get() + bottom_bar_height as usize,
            ),
            self.min_scroll_bar_height() as usize,
        );
        self.set_viewport(pane.pane_id(), Some(row), dims);
        context.invalidate();
        self.dragging.replace((item, start_event));
    }

    fn drag_ui_item(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        x: usize,
        y: i64,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match item.item_type {
            UIItemType::Split(split) => {
                self.drag_split(item, split, start_event, x, y, context);
            }
            UIItemType::ScrollThumb => {
                self.drag_scroll_thumb(item, start_event, event, context);
            }
            UIItemType::TabSidebar(TabSidebarItem::ResizeHandle) => {
                self.drag_sidebar_resize(item, start_event, event, context);
            }
            _ => {
                log::error!("drag not implemented for {:?}", item);
            }
        }
    }

    fn mouse_event_ui_item(
        &mut self,
        item: UIItem,
        pane: Arc<dyn Pane>,
        _y: i64,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        self.last_ui_item.replace(item.clone());
        match item.item_type {
            UIItemType::TabBar(item) => {
                self.mouse_event_tab_bar(item, event, context);
            }
            UIItemType::AboveScrollThumb => {
                self.mouse_event_above_scroll_thumb(item, pane, event, context);
            }
            UIItemType::ScrollThumb => {
                self.mouse_event_scroll_thumb(item, pane, event, context);
            }
            UIItemType::BelowScrollThumb => {
                self.mouse_event_below_scroll_thumb(item, pane, event, context);
            }
            UIItemType::Split(split) => {
                self.mouse_event_split(item, split, event, context);
            }
            UIItemType::CloseTab(idx) => {
                self.mouse_event_close_tab(idx, event, context);
            }
            UIItemType::ProfileDropdownItem(idx) => {
                self.mouse_event_profile_dropdown_item(idx, event, context);
            }
            UIItemType::TabSidebar(ref sidebar_item) => {
                self.mouse_event_tab_sidebar(item.clone(), sidebar_item.clone(), event, context);
            }
        }
    }

    pub fn mouse_event_close_tab(
        &mut self,
        idx: usize,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match event.kind {
            WMEK::Press(MousePress::Left) => {
                log::debug!("Should close tab {}", idx);
                self.close_specific_tab(idx, true);
                context.set_cursor(Some(MouseCursor::Arrow));
            }
            WMEK::Move => {
                context.set_cursor(Some(MouseCursor::Hand));
            }
            _ => {
                context.set_cursor(Some(MouseCursor::Arrow));
            }
        }
    }

    fn do_new_tab_button_click(&mut self, button: MousePress) {
        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };
        // Left click: use custom default profile if set, otherwise current domain
        // Right click: show full launcher
        let action = match button {
            MousePress::Left => Some(
                self.default_profile_action
                    .clone()
                    .unwrap_or(KeyAssignment::SpawnTab(SpawnTabDomain::CurrentPaneDomain)),
            ),
            MousePress::Right => Some(KeyAssignment::ShowLauncher),
            MousePress::Middle => None,
        };
        if let Some(assignment) = action {
            let window = GuiWin::new(self);
            let pane_id = pane.pane_id();
            window.window.notify(TermWindowNotif::PerformAssignment {
                pane_id,
                assignment,
                tx: None,
            });
        }
    }

    fn do_new_tab_dropdown_click(&mut self) {
        // Find the chevron button's UIItem to anchor the dropdown position
        let anchor = match self.last_ui_item.clone() {
            Some(item) => item,
            None => return,
        };
        let modal = crate::termwindow::profile_dropdown::ProfileDropdown::new(self, &anchor);
        self.set_modal(std::rc::Rc::new(modal));
    }

    fn mouse_event_profile_dropdown_item(
        &mut self,
        idx: usize,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        use crate::termwindow::profile_dropdown::ProfileDropdown;

        match event.kind {
            WMEK::Press(MousePress::Left) => {
                // Left click: spawn tab with this profile
                if let Some(modal) = self.get_modal() {
                    if let Some(dropdown) = modal.downcast_ref::<ProfileDropdown>() {
                        if let Some(entry) = dropdown.entries.get(idx) {
                            let action = entry.action.clone();
                            self.cancel_modal();
                            if let Some(pane) = self.get_active_pane_or_overlay() {
                                if let Err(err) = self.perform_key_assignment(&pane, &action) {
                                    log::error!("Error spawning from profile dropdown: {err:#}");
                                }
                            }
                        }
                    }
                }
            }
            WMEK::Press(MousePress::Right) => {
                // Right click: set as default profile for "+" button
                if let Some(modal) = self.get_modal() {
                    if let Some(dropdown) = modal.downcast_ref::<ProfileDropdown>() {
                        if let Some(entry) = dropdown.entries.get(idx) {
                            self.default_profile_action = Some(entry.action.clone());
                            log::info!(
                                "Default profile set to: {}",
                                entry.label
                            );
                            self.cancel_modal();
                            context.invalidate();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    pub fn mouse_event_tab_bar(
        &mut self,
        item: TabBarItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match event.kind {
            WMEK::Press(MousePress::Left) => match item {
                TabBarItem::RemoteAccess { .. } => {
                    self.toggle_remote_access();
                }
                TabBarItem::Tab { tab_idx, .. } => {
                    // Capture the currently active tab before switching —
                    // this is the tab we'll drop into if a drag occurs.
                    let dest_tab_id = Mux::get()
                        .get_active_tab_for_window(self.mux_window_id)
                        .map(|t| t.tab_id());
                    self.activate_tab(tab_idx as isize).ok();
                    if let Some(dest_tab_id) = dest_tab_id {
                        self.tab_drag = Some(TabDragState {
                            source_tab_idx: tab_idx,
                            dest_tab_id,
                            target_pane: None,
                            target_zone: None,
                            start_coords: (event.coords.x, event.coords.y),
                            threshold_exceeded: false,
                        });
                    }
                }
                TabBarItem::NewTabButton { .. } => {
                    self.do_new_tab_button_click(MousePress::Left);
                }
                TabBarItem::NewTabDropdown => {
                    self.do_new_tab_dropdown_click();
                }
                TabBarItem::None | TabBarItem::LeftStatus | TabBarItem::RightStatus => {
                    let maximized = self
                        .window_state
                        .intersects(WindowState::MAXIMIZED | WindowState::FULL_SCREEN);
                    if let Some(ref window) = self.window {
                        if self.config.window_decorations
                            == WindowDecorations::INTEGRATED_BUTTONS | WindowDecorations::RESIZE
                        {
                            if self.last_mouse_click.as_ref().map(|c| c.streak) == Some(2) {
                                if maximized {
                                    window.restore();
                                } else {
                                    window.maximize();
                                }
                            }
                        }
                    }
                    // Potentially starting a drag by the tab bar
                    if !maximized {
                        self.window_drag_position.replace(event.clone());
                    }
                    context.request_drag_move();
                }
                TabBarItem::WindowButton(button) => {
                    use window::IntegratedTitleButton as Button;
                    if let Some(ref window) = self.window {
                        match button {
                            Button::Hide => window.hide(),
                            Button::Maximize => {
                                let maximized = self
                                    .window_state
                                    .intersects(WindowState::MAXIMIZED | WindowState::FULL_SCREEN);
                                if maximized {
                                    window.restore();
                                } else {
                                    window.maximize();
                                }
                            }
                            Button::Close => self.close_requested(&window.clone()),
                        }
                    }
                }
            },
            WMEK::Press(MousePress::Middle) => match item {
                TabBarItem::Tab { tab_idx, .. } => {
                    self.close_specific_tab(tab_idx, true);
                }
                TabBarItem::NewTabButton { .. } => {
                    self.do_new_tab_button_click(MousePress::Middle);
                }
                TabBarItem::None
                | TabBarItem::LeftStatus
                | TabBarItem::RightStatus
                | TabBarItem::NewTabDropdown
                | TabBarItem::RemoteAccess { .. }
                | TabBarItem::WindowButton(_) => {}
            },
            WMEK::Press(MousePress::Right) => match item {
                TabBarItem::RemoteAccess { .. } => {
                    if self.web_server_handle.is_some() {
                        self.copy_remote_url_to_clipboard();
                    }
                }
                TabBarItem::Tab { .. } => {
                    self.show_tab_navigator();
                }
                TabBarItem::NewTabButton { .. } => {
                    self.do_new_tab_button_click(MousePress::Right);
                }
                TabBarItem::NewTabDropdown => {
                    self.do_new_tab_dropdown_click();
                }
                TabBarItem::None
                | TabBarItem::LeftStatus
                | TabBarItem::RightStatus
                | TabBarItem::WindowButton(_) => {}
            },
            WMEK::Move => match item {
                TabBarItem::None | TabBarItem::LeftStatus | TabBarItem::RightStatus => {
                    context.set_window_drag_position(event.screen_coords);
                }
                TabBarItem::WindowButton(window::IntegratedTitleButton::Maximize) => {
                    let item = self.last_ui_item.clone().unwrap();
                    let bounds: ::window::ScreenRect = euclid::rect(
                        item.x as isize - (event.coords.x as isize - event.screen_coords.x),
                        item.y as isize - (event.coords.y as isize - event.screen_coords.y),
                        item.width as isize,
                        item.height as isize,
                    );
                    context.set_maximize_button_position(bounds);
                }
                TabBarItem::WindowButton(_)
                | TabBarItem::Tab { .. }
                | TabBarItem::NewTabButton { .. }
                | TabBarItem::NewTabDropdown
                | TabBarItem::RemoteAccess { .. } => {}
            },
            WMEK::VertWheel(n) => {
                if self.config.mouse_wheel_scrolls_tabs {
                    self.activate_tab_relative(if n < 1 { 1 } else { -1 }, true)
                        .ok();
                }
            }
            _ => {}
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_above_scroll_thumb(
        &mut self,
        _item: UIItem,
        pane: Arc<dyn Pane>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if let WMEK::Press(MousePress::Left) = event.kind {
            let dims = pane.get_dimensions();
            let current_viewport = self.get_viewport(pane.pane_id());
            // Page up
            self.set_viewport(
                pane.pane_id(),
                Some(
                    current_viewport
                        .unwrap_or(dims.physical_top)
                        .saturating_sub(self.terminal_size.rows.try_into().unwrap()),
                ),
                dims,
            );
            context.invalidate();
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_below_scroll_thumb(
        &mut self,
        _item: UIItem,
        pane: Arc<dyn Pane>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if let WMEK::Press(MousePress::Left) = event.kind {
            let dims = pane.get_dimensions();
            let current_viewport = self.get_viewport(pane.pane_id());
            // Page down
            self.set_viewport(
                pane.pane_id(),
                Some(
                    current_viewport
                        .unwrap_or(dims.physical_top)
                        .saturating_add(self.terminal_size.rows.try_into().unwrap()),
                ),
                dims,
            );
            context.invalidate();
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_scroll_thumb(
        &mut self,
        item: UIItem,
        _pane: Arc<dyn Pane>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if let WMEK::Press(MousePress::Left) = event.kind {
            // Start a scroll drag
            // self.scroll_drag_start = Some(from_top);
            self.dragging = Some((item, event));
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_split(
        &mut self,
        item: UIItem,
        split: PositionedSplit,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(match &split.direction {
            SplitDirection::Horizontal => MouseCursor::SizeLeftRight,
            SplitDirection::Vertical => MouseCursor::SizeUpDown,
        }));

        if event.kind == WMEK::Press(MousePress::Left) {
            self.dragging.replace((item, event));
        }
    }

    fn mouse_event_terminal(
        &mut self,
        mut pane: Arc<dyn Pane>,
        position: ClickPosition,
        event: MouseEvent,
        context: &dyn WindowOps,
        capture_mouse: bool,
    ) {
        let mut is_click_to_focus_pane = false;

        let ClickPosition {
            mut column,
            mut row,
            mut x_pixel_offset,
            mut y_pixel_offset,
        } = position;

        let is_already_captured = matches!(
            self.current_mouse_capture,
            Some(MouseCapture::TerminalPane(_))
        );

        // Hit-test using base metrics since pos.top/pos.left are in base cell
        // units. Compute base-metric row/column for pane hit-testing.
        let base_cell_w = self.render_metrics.cell_size.width;
        let base_cell_h = self.render_metrics.cell_size.height;

        // Compute top_pixel_y and left_pixel_x matching the rendering path,
        // for pixel-based pane-local coordinate recomputation below.
        let tab_bar_h = if self.show_tab_bar && !self.config.tab_bar_at_bottom {
            self.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };
        let (pad_l, pad_t) = self.padding_left_top();
        let border_for_pane = self.get_os_border();
        let render_top_pixel_y = tab_bar_h + pad_t + border_for_pane.top.get() as f32;
        let sidebar_off = self.sidebar_x_offset();
        let render_left_pixel_x = pad_l + border_for_pane.left.get() as f32 + sidebar_off;

        // Base-metric cell coordinates for hit-testing against pos.top/pos.left
        let base_row = ((event.coords.y as f32 - render_top_pixel_y).max(0.)
            as isize / base_cell_h) as i64;
        let base_col = ((event.coords.x as f32 - render_left_pixel_x).max(0.)
            as isize / base_cell_w) as usize;

        for pos in self.get_panes_to_render() {
            if !is_already_captured
                && base_row >= pos.top as i64
                && base_row <= (pos.top + pos.height) as i64
                && base_col >= pos.left
                && base_col <= pos.left + pos.width
            {
                if pane.pane_id() != pos.pane.pane_id() {
                    // We're over a pane that isn't active
                    match &event.kind {
                        WMEK::Press(_) => {
                            let mux = Mux::get();
                            mux.get_active_tab_for_window(self.mux_window_id)
                                .map(|tab| tab.set_active_idx(pos.index));

                            pane = Arc::clone(&pos.pane);
                            is_click_to_focus_pane = true;
                        }
                        WMEK::Move => {
                            if self.config.pane_focus_follows_mouse {
                                let mux = Mux::get();
                                mux.get_active_tab_for_window(self.mux_window_id)
                                    .map(|tab| tab.set_active_idx(pos.index));

                                pane = Arc::clone(&pos.pane);
                                context.invalidate();
                            }
                        }
                        WMEK::Release(_) | WMEK::HorzWheel(_) => {}
                        WMEK::VertWheel(_) => {
                            // Let wheel events route to the hovered pane,
                            // even if it doesn't have focus
                            pane = Arc::clone(&pos.pane);
                            context.invalidate();
                        }
                    }
                }

                // Recompute pane-local cell coordinates using the TARGET pane's
                // scaled cell size, starting from the pane's pixel position.
                // This correctly handles the case where pos.top/pos.left are in
                // base cell units but the pane has a different font scale.
                let target_scale = self.pane_font_scale(pos.pane.pane_id());
                let target_cell = if (target_scale - 1.0).abs() > 0.001 {
                    let key = Self::font_scale_key(target_scale);
                    self.scaled_font_configs
                        .get(&key)
                        .map(|(_, m)| m.cell_size)
                        .unwrap_or(self.render_metrics.cell_size)
                } else {
                    self.render_metrics.cell_size
                };
                let pane_pixel_y = event.coords.y as f32
                    - render_top_pixel_y
                    - pos.top as f32 * base_cell_h as f32;
                let pane_pixel_x = event.coords.x as f32
                    - render_left_pixel_x
                    - pos.left as f32 * base_cell_w as f32;
                row = (pane_pixel_y.max(0.) as isize / target_cell.height) as i64;
                column = {
                    let fx = pane_pixel_x.max(0.) / target_cell.width as f32;
                    if !pane.is_mouse_grabbed() { fx.round() } else { fx }
                        .trunc() as usize
                };
                y_pixel_offset = if row > 0 {
                    (pane_pixel_y.max(0.) as isize) % target_cell.height
                } else {
                    pane_pixel_y as isize
                };
                x_pixel_offset = if column > 0 {
                    (pane_pixel_x.max(0.) as isize) % target_cell.width
                } else {
                    pane_pixel_x as isize
                };
                break;
            } else if is_already_captured && pane.pane_id() == pos.pane.pane_id() {
                // Recompute pane-local coordinates for the captured pane too
                let target_scale = self.pane_font_scale(pos.pane.pane_id());
                let target_cell = if (target_scale - 1.0).abs() > 0.001 {
                    let key = Self::font_scale_key(target_scale);
                    self.scaled_font_configs
                        .get(&key)
                        .map(|(_, m)| m.cell_size)
                        .unwrap_or(self.render_metrics.cell_size)
                } else {
                    self.render_metrics.cell_size
                };
                let pane_pixel_y = event.coords.y as f32
                    - render_top_pixel_y
                    - pos.top as f32 * base_cell_h as f32;
                let pane_pixel_x = event.coords.x as f32
                    - render_left_pixel_x
                    - pos.left as f32 * base_cell_w as f32;
                row = (pane_pixel_y as isize / target_cell.height).max(0) as i64;
                column = (pane_pixel_x.max(0.) / target_cell.width as f32)
                    .trunc() as usize;

                if pane_pixel_x < 0. {
                    x_pixel_offset = pane_pixel_x as isize;
                } else {
                    x_pixel_offset = (pane_pixel_x as isize) % target_cell.width;
                }
                if pane_pixel_y < 0. {
                    y_pixel_offset = pane_pixel_y as isize;
                } else {
                    y_pixel_offset = (pane_pixel_y as isize) % target_cell.height;
                }

                break;
            }
        }

        if capture_mouse {
            self.current_mouse_capture = Some(MouseCapture::TerminalPane(pane.pane_id()));
        }

        let is_focused = if let Some(focused) = self.focused.as_ref() {
            !self.config.swallow_mouse_click_on_window_focus
                || (focused.elapsed() > Duration::from_millis(200))
        } else {
            false
        };

        if self.focused.is_some() && !is_focused {
            if matches!(&event.kind, WMEK::Press(_))
                && self.config.swallow_mouse_click_on_window_focus
            {
                // Entering click to focus state
                self.is_click_to_focus_window = true;
                context.invalidate();
                log::trace!("enter click to focus");
                return;
            }
        }
        if self.is_click_to_focus_window && matches!(&event.kind, WMEK::Release(_)) {
            // Exiting click to focus state
            self.is_click_to_focus_window = false;
            context.invalidate();
            log::trace!("exit click to focus");
            return;
        }

        let allow_action = if self.is_click_to_focus_window || !is_focused {
            matches!(&event.kind, WMEK::VertWheel(_) | WMEK::HorzWheel(_))
        } else {
            true
        };

        log::trace!(
            "is_focused={} allow_action={} event={:?}",
            is_focused,
            allow_action,
            event
        );

        let dims = pane.get_dimensions();
        let stable_row = self
            .get_viewport(pane.pane_id())
            .unwrap_or(dims.physical_top)
            + row as StableRowIndex;

        self.pane_state(pane.pane_id())
            .mouse_terminal_coords
            .replace((
                ClickPosition {
                    column,
                    row,
                    x_pixel_offset,
                    y_pixel_offset,
                },
                stable_row,
            ));

        pane.apply_hyperlinks(stable_row..stable_row + 1, &self.config.hyperlink_rules);

        struct FindCurrentLink {
            current: Option<Arc<Hyperlink>>,
            stable_row: StableRowIndex,
            column: usize,
        }

        impl WithPaneLines for FindCurrentLink {
            fn with_lines_mut(&mut self, stable_top: StableRowIndex, lines: &mut [&mut Line]) {
                if stable_top == self.stable_row {
                    if let Some(line) = lines.get(0) {
                        if let Some(cell) = line.get_cell(self.column) {
                            self.current = cell.attrs().hyperlink().cloned();
                        }
                    }
                }
            }
        }

        let mut find_link = FindCurrentLink {
            current: None,
            stable_row,
            column,
        };
        pane.with_lines_mut(stable_row..stable_row + 1, &mut find_link);
        let new_highlight = find_link.current;

        match (self.current_highlight.as_ref(), new_highlight) {
            (Some(old_link), Some(new_link)) if Arc::ptr_eq(&old_link, &new_link) => {
                // Unchanged
            }
            (None, None) => {
                // Unchanged
            }
            (_, rhs) => {
                // We're hovering over a different URL, so invalidate and repaint
                // so that we render the underline correctly
                self.current_highlight = rhs;
                context.invalidate();
            }
        };

        let outside_window = event.coords.x < 0
            || event.coords.x as usize > self.dimensions.pixel_width
            || event.coords.y < 0
            || event.coords.y as usize > self.dimensions.pixel_height;

        context.set_cursor(Some(
            if self.pane_long_press.as_ref().map_or(false, |lp| lp.revealed) {
                let pane_id = self.pane_long_press.as_ref().unwrap().pane_id;
                if self.overlay_button_at(&event, pane_id).is_some() {
                    MouseCursor::Hand
                } else {
                    MouseCursor::Arrow
                }
            } else if self.current_highlight.is_some() {
                // When hovering over a hyperlink, show an appropriate
                // mouse cursor to give the cue that it is clickable
                MouseCursor::Hand
            } else if pane.is_mouse_grabbed() || outside_window {
                MouseCursor::Arrow
            } else {
                MouseCursor::Text
            },
        ));

        let event_trigger_type = match &event.kind {
            WMEK::Press(press) => {
                let press = mouse_press_to_tmb(press);
                match self.last_mouse_click.as_ref() {
                    Some(LastMouseClick { streak, button, .. }) if *button == press => {
                        Some(MouseEventTrigger::Down {
                            streak: *streak,
                            button: press,
                        })
                    }
                    _ => None,
                }
            }
            WMEK::Release(press) => {
                let press = mouse_press_to_tmb(press);
                match self.last_mouse_click.as_ref() {
                    Some(LastMouseClick { streak, button, .. }) if *button == press => {
                        Some(MouseEventTrigger::Up {
                            streak: *streak,
                            button: press,
                        })
                    }
                    _ => None,
                }
            }
            WMEK::Move => {
                if !self.current_mouse_buttons.is_empty() {
                    if let Some(LastMouseClick { streak, button, .. }) =
                        self.last_mouse_click.as_ref()
                    {
                        if Some(*button)
                            == self.current_mouse_buttons.last().map(mouse_press_to_tmb)
                        {
                            Some(MouseEventTrigger::Drag {
                                streak: *streak,
                                button: *button,
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            WMEK::VertWheel(amount) => Some(match *amount {
                0 => return,
                1.. => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelUp(*amount as usize),
                },
                _ => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelDown(-amount as usize),
                },
            }),
            WMEK::HorzWheel(amount) => Some(match *amount {
                0 => return,
                1.. => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelLeft(*amount as usize),
                },
                _ => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelRight(-amount as usize),
                },
            }),
        };

        if allow_action {
            if let Some(mut event_trigger_type) = event_trigger_type {
                self.current_event = Some(event_trigger_type.to_dynamic());
                let mut modifiers = event.modifiers;

                // Since we use shift to force assessing the mouse bindings, pretend
                // that shift is not one of the mods when the mouse is grabbed.
                let mut mouse_reporting = pane.is_mouse_grabbed();
                if mouse_reporting {
                    if modifiers.contains(self.config.bypass_mouse_reporting_modifiers) {
                        modifiers.remove(self.config.bypass_mouse_reporting_modifiers);
                        mouse_reporting = false;
                    }
                }

                if mouse_reporting {
                    // If they were scrolled back prior to launching an
                    // application that captures the mouse, then mouse based
                    // scrolling assignments won't have any effect.
                    // Ensure that we scroll to the bottom if they try to
                    // use the mouse so that things are less surprising
                    self.scroll_to_bottom(&pane);
                }

                // normalize delta and streak to make mouse assignment
                // easier to wrangle
                match event_trigger_type {
                    MouseEventTrigger::Down {
                        ref mut streak,
                        button:
                            MouseButton::WheelUp(ref mut delta)
                            | MouseButton::WheelDown(ref mut delta)
                            | MouseButton::WheelLeft(ref mut delta)
                            | MouseButton::WheelRight(ref mut delta),
                    }
                    | MouseEventTrigger::Up {
                        ref mut streak,
                        button:
                            MouseButton::WheelUp(ref mut delta)
                            | MouseButton::WheelDown(ref mut delta)
                            | MouseButton::WheelLeft(ref mut delta)
                            | MouseButton::WheelRight(ref mut delta),
                    }
                    | MouseEventTrigger::Drag {
                        ref mut streak,
                        button:
                            MouseButton::WheelUp(ref mut delta)
                            | MouseButton::WheelDown(ref mut delta)
                            | MouseButton::WheelLeft(ref mut delta)
                            | MouseButton::WheelRight(ref mut delta),
                    } => {
                        *streak = 1;
                        *delta = 1;
                    }
                    _ => {}
                };

                let mouse_mods = config::MouseEventTriggerMods {
                    mods: modifiers,
                    mouse_reporting,
                    alt_screen: if pane.is_alt_screen_active() {
                        MouseEventAltScreen::True
                    } else {
                        MouseEventAltScreen::False
                    },
                };

                if let Some(action) = self.input_map.lookup_mouse(event_trigger_type, mouse_mods) {
                    self.perform_key_assignment(&pane, &action).ok();
                    return;
                }
            }
        }

        let mouse_event = terminaler_term::MouseEvent {
            kind: match event.kind {
                WMEK::Move => TMEK::Move,
                WMEK::VertWheel(_) | WMEK::HorzWheel(_) | WMEK::Press(_) => TMEK::Press,
                WMEK::Release(_) => TMEK::Release,
            },
            button: match event.kind {
                WMEK::Release(ref press) | WMEK::Press(ref press) => mouse_press_to_tmb(press),
                WMEK::Move => {
                    if event.mouse_buttons == WMB::LEFT {
                        TMB::Left
                    } else if event.mouse_buttons == WMB::RIGHT {
                        TMB::Right
                    } else if event.mouse_buttons == WMB::MIDDLE {
                        TMB::Middle
                    } else {
                        TMB::None
                    }
                }
                WMEK::VertWheel(amount) => {
                    if amount > 0 {
                        TMB::WheelUp(amount as usize)
                    } else {
                        TMB::WheelDown((-amount) as usize)
                    }
                }
                WMEK::HorzWheel(amount) => {
                    if amount > 0 {
                        TMB::WheelLeft(amount as usize)
                    } else {
                        TMB::WheelRight((-amount) as usize)
                    }
                }
            },
            x: column,
            y: row,
            x_pixel_offset,
            y_pixel_offset,
            modifiers: event.modifiers,
        };

        if allow_action
            && !(self.config.swallow_mouse_click_on_pane_focus && is_click_to_focus_pane)
        {
            pane.mouse_event(mouse_event).ok();
        }

        match event.kind {
            WMEK::Move => {}
            _ => {
                context.invalidate();
            }
        }
    }

    fn update_tab_drag_target(&mut self, event: &MouseEvent) {
        if self.tab_drag.is_none() {
            return;
        }

        let panes = self.get_panes_to_render();
        let (padding_left, padding_top) = self.padding_left_top();

        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height().unwrap_or(0.)
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

        let mouse_x = event.coords.x as f32;
        let mouse_y = event.coords.y as f32;

        let mut found_pane = None;
        let mut found_zone = None;
        let sidebar_off = self.sidebar_x_offset();

        for pos in &panes {
            let pane_left = padding_left + border.left.get() as f32 + sidebar_off
                + (pos.left as f32 * cell_width);
            let pane_top = top_pixel_y + (pos.top as f32 * cell_height);
            let pane_width = pos.width as f32 * cell_width;
            let pane_height = pos.height as f32 * cell_height;

            if mouse_x >= pane_left
                && mouse_x < pane_left + pane_width
                && mouse_y >= pane_top
                && mouse_y < pane_top + pane_height
            {
                let zone = compute_drop_zone(
                    mouse_x, mouse_y,
                    pane_left, pane_top,
                    pane_width, pane_height,
                );
                found_pane = Some(pos.pane.pane_id());
                found_zone = Some(zone);
                break;
            }
        }

        if let Some(ref mut tab_drag) = self.tab_drag {
            tab_drag.target_pane = found_pane;
            tab_drag.target_zone = found_zone;
        }
    }

    fn execute_tab_drag_drop(&mut self, tab_drag: TabDragState) {
        let target_pane_id = match tab_drag.target_pane {
            Some(id) => id,
            None => return,
        };
        let zone = match tab_drag.target_zone {
            Some(z) => z,
            None => return,
        };

        let mux = Mux::get();

        // Look up source and destination tabs
        let source_tab;
        let dest_tab;
        {
            let mux_window = match mux.get_window(self.mux_window_id) {
                Some(w) => w,
                None => return,
            };

            source_tab = match mux_window.get_by_idx(tab_drag.source_tab_idx) {
                Some(tab) => Arc::clone(tab),
                None => return,
            };

            // Find the destination tab by the ID we saved before the drag
            dest_tab = mux_window
                .idx_by_id(tab_drag.dest_tab_id)
                .and_then(|idx| mux_window.get_by_idx(idx).map(Arc::clone));
        }

        let dest_tab = match dest_tab {
            Some(tab) => tab,
            None => return,
        };

        // Don't drop onto the same tab
        if source_tab.tab_id() == dest_tab.tab_id() {
            return;
        }

        // Don't drop onto a zoomed tab
        if dest_tab.get_zoomed_pane().is_some() {
            return;
        }

        // Get the active pane from the source tab (the one we're moving)
        let source_pane = match source_tab.get_active_pane() {
            Some(pane) => pane,
            None => return,
        };
        let source_pane_id = source_pane.pane_id();

        // Find the target pane index in the destination tab
        let target_pane_index = {
            let panes = dest_tab.iter_panes();
            match panes.iter().find(|p| p.pane.pane_id() == target_pane_id) {
                Some(p) => p.index,
                None => return,
            }
        };

        // Build split request from drop zone
        let request = SplitRequest {
            direction: match zone {
                DropZone::Left | DropZone::Right => SplitDirection::Horizontal,
                DropZone::Top | DropZone::Bottom => SplitDirection::Vertical,
            },
            target_is_second: match zone {
                DropZone::Right | DropZone::Bottom => true,
                DropZone::Left | DropZone::Top => false,
            },
            top_level: false,
            size: SplitSize::Percent(50),
        };

        // Remove the pane from its source tab (doesn't kill it)
        let pane = match source_tab.remove_pane(source_pane_id) {
            Some(pane) => pane,
            None => return,
        };

        // Insert into the target split
        if let Err(err) = dest_tab.split_and_insert(target_pane_index, request, pane) {
            log::error!("Failed to split_and_insert during tab drag: {:#}", err);
            return;
        }

        // If the source tab is now dead (no panes left), remove it
        let source_tab_id = source_tab.tab_id();
        if source_tab.is_dead() {
            mux.remove_tab(source_tab_id);
        }
    }

    fn start_pane_overlay(&mut self, event: &MouseEvent) {
        let pane_id = match self.pane_id_at_pixel_coords(event.coords.x, event.coords.y) {
            Some(id) => id,
            None => return,
        };

        self.toast_expanded_for = None;
        self.pane_long_press = Some(super::PaneLongPress {
            pane_id,
            revealed: true,
        });
    }

    fn pane_id_at_pixel_coords(&self, px: isize, py: isize) -> Option<mux::pane::PaneId> {
        let panes = self.get_panes_to_render();
        let (padding_left, padding_top) = self.padding_left_top();

        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height().unwrap_or(0.)
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

        let mx = px as f32;
        let my = py as f32;

        let sidebar_off = self.sidebar_x_offset();
        for pos in &panes {
            let pane_left =
                padding_left + border.left.get() as f32 + sidebar_off + (pos.left as f32 * cell_width);
            let pane_top = top_pixel_y + (pos.top as f32 * cell_height);
            let pane_width = pos.width as f32 * cell_width;
            let pane_height = pos.height as f32 * cell_height;

            if mx >= pane_left
                && mx < pane_left + pane_width
                && my >= pane_top
                && my < pane_top + pane_height
            {
                return Some(pos.pane.pane_id());
            }
        }
        None
    }

    fn execute_pane_to_tab(&mut self, pane_id: mux::pane::PaneId) {
        let mux_window_id = self.mux_window_id;
        promise::spawn::spawn(async move {
            let mux = Mux::get();
            if let Err(err) = mux
                .move_pane_to_new_tab(pane_id, Some(mux_window_id), None)
                .await
            {
                log::error!("Failed to move pane to new tab: {:#}", err);
            }
        })
        .detach();
    }

    fn overlay_button_at(
        &self,
        event: &MouseEvent,
        pane_id: mux::pane::PaneId,
    ) -> Option<&'static str> {
        const BUTTON_NAMES: [&str; 9] = [
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

        let panes = self.get_panes_to_render();
        let (padding_left, padding_top) = self.padding_left_top();

        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height().unwrap_or(0.)
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

        let mx = event.coords.x as f32;
        let my = event.coords.y as f32;

        let sidebar_off = self.sidebar_x_offset();
        for pos in &panes {
            if pos.pane.pane_id() != pane_id {
                continue;
            }
            let pane_left =
                padding_left + border.left.get() as f32 + sidebar_off + (pos.left as f32 * cell_width);
            let pane_top = top_pixel_y + (pos.top as f32 * cell_height);
            let pane_width = pos.width as f32 * cell_width;
            let pane_height = pos.height as f32 * cell_height;

            let btn_size = 60.0f32;
            let gap = 8.0f32;
            let cols = 3usize;
            let rows = 3usize;
            let grid_w = cols as f32 * btn_size + (cols - 1) as f32 * gap;
            let grid_h = rows as f32 * btn_size + (rows - 1) as f32 * gap;
            let cx = pane_left + pane_width / 2.0;
            let cy = pane_top + pane_height / 2.0;
            let grid_left = cx - grid_w / 2.0;
            let grid_top = cy - grid_h / 2.0;

            // Check if inside grid bounds
            if mx < grid_left || mx >= grid_left + grid_w
                || my < grid_top || my >= grid_top + grid_h
            {
                return None;
            }

            let rel_x = mx - grid_left;
            let rel_y = my - grid_top;
            let col = (rel_x / (btn_size + gap)) as usize;
            let row = (rel_y / (btn_size + gap)) as usize;

            // Check we're inside a button, not in a gap
            let btn_left = col as f32 * (btn_size + gap);
            let btn_top_y = row as f32 * (btn_size + gap);
            if rel_x > btn_left + btn_size || rel_y > btn_top_y + btn_size {
                return None;
            }

            let idx = row * cols + col;
            return BUTTON_NAMES.get(idx).copied();
        }
        None
    }

    fn toast_button_at(
        &self,
        event: &MouseEvent,
    ) -> Option<(mux::pane::PaneId, &'static str)> {
        use crate::termwindow::render::pane::{
            TOAST_BTN_SIZE, TOAST_BUTTON_NAMES, TOAST_COLLAPSED_WIDTH, TOAST_COUNT, TOAST_GAP,
            TOAST_HEIGHT, TOAST_MIN_PANE_HEIGHT, TOAST_MIN_PANE_WIDTH, TOAST_PADDING, TOAST_WIDTH,
        };

        let hovered_id = self.hovered_pane_id?;

        // Don't hit-test if long-press overlay is active on this pane
        if self
            .pane_long_press
            .as_ref()
            .map_or(false, |lp| lp.revealed && lp.pane_id == hovered_id)
        {
            return None;
        }

        let panes = self.get_panes_to_render();
        let pos = panes.iter().find(|p| p.pane.pane_id() == hovered_id)?;

        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;
        let (padding_left, padding_top) = self.padding_left_top();

        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height().unwrap_or(0.)
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

        // Visual bounds matching paint_toast_toolbar (same as build_pane background_rect)
        let sidebar_off = self.sidebar_x_offset();
        let (bg_x, width_delta) = if pos.left == 0 {
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
            bg_x + (pos.width as f32 * cell_width) + width_delta
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

        // Same size guard as rendering
        if pane_visual_width < TOAST_MIN_PANE_WIDTH || pane_visual_height < TOAST_MIN_PANE_HEIGHT {
            return None;
        }

        let mx = event.coords.x as f32;
        let my = event.coords.y as f32;

        let is_expanded = self.toast_expanded_for == Some(hovered_id)
            && pane_visual_width >= TOAST_WIDTH + 10.0;

        // Must match the offset used in paint_toast_toolbar
        let toast_top_offset = 10.0f32;

        if is_expanded {
            // --- Expanded: full 9-button hit-test ---
            let toast_left = bg_right - TOAST_WIDTH;
            let toast_top = bg_y + toast_top_offset;

            if mx < toast_left
                || mx >= toast_left + TOAST_WIDTH
                || my < toast_top
                || my >= toast_top + TOAST_HEIGHT
            {
                return None;
            }

            let rel_x = mx - toast_left - TOAST_PADDING;
            let rel_y = my - toast_top - TOAST_PADDING;

            if rel_x < 0.0 || rel_y < 0.0 || rel_y >= TOAST_BTN_SIZE {
                return None;
            }

            let idx = (rel_x / (TOAST_BTN_SIZE + TOAST_GAP)) as usize;
            if idx >= TOAST_COUNT {
                return None;
            }

            let btn_left = idx as f32 * (TOAST_BTN_SIZE + TOAST_GAP);
            if rel_x > btn_left + TOAST_BTN_SIZE {
                return None;
            }

            Some((hovered_id, TOAST_BUTTON_NAMES[idx]))
        } else {
            // --- Collapsed: single trigger pill ---
            let pill_left = bg_right - TOAST_COLLAPSED_WIDTH;
            let pill_top = bg_y + toast_top_offset;

            if mx >= pill_left
                && mx < pill_left + TOAST_COLLAPSED_WIDTH
                && my >= pill_top
                && my < pill_top + TOAST_HEIGHT
            {
                Some((hovered_id, "trigger"))
            } else {
                None
            }
        }
    }

    pub fn mouse_event_tab_sidebar(
        &mut self,
        item: UIItem,
        sidebar_item: TabSidebarItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match event.kind {
            WMEK::Press(MousePress::Left) => match sidebar_item {
                TabSidebarItem::Tab { tab_idx, .. } => {
                    self.activate_tab(tab_idx as isize).ok();
                }
                TabSidebarItem::Pane { tab_idx, pane_idx } => {
                    self.activate_tab(tab_idx as isize).ok();
                    let mux = Mux::get();
                    if let Some(tab) = mux.get_active_tab_for_window(self.mux_window_id) {
                        tab.set_active_idx(pane_idx);
                        // Clear notifications for the activated pane
                        let panes = tab.iter_panes();
                        if let Some(pos) = panes.iter().find(|p| p.index == pane_idx) {
                            let pid = pos.pane.pane_id();
                            let mut ps = self.pane_state(pid);
                            ps.notification_start = None;
                            ps.notification_count = 0;
                        }
                    }
                }
                TabSidebarItem::ClosePane { pane_id } => {
                    self.close_pane_by_id(pane_id as mux::pane::PaneId, true);
                }
                TabSidebarItem::NewTabButton => {
                    self.spawn_tab(&SpawnTabDomain::CurrentPaneDomain);
                }
                TabSidebarItem::ResizeHandle => {
                    context.set_cursor(Some(MouseCursor::SizeLeftRight));
                    self.dragging.replace((item.clone(), event));
                }
            },
            WMEK::Press(MousePress::Middle) => match sidebar_item {
                TabSidebarItem::Tab { tab_idx, .. } => {
                    self.close_specific_tab(tab_idx, true);
                }
                TabSidebarItem::Pane { pane_idx, tab_idx } => {
                    self.activate_tab(tab_idx as isize).ok();
                    let mux = Mux::get();
                    if let Some(tab) = mux.get_active_tab_for_window(self.mux_window_id) {
                        let panes = tab.iter_panes();
                        if let Some(pos) = panes.iter().find(|p| p.index == pane_idx) {
                            self.close_pane_by_id(pos.pane.pane_id(), true);
                        }
                    }
                }
                _ => {}
            },
            WMEK::Move => match sidebar_item {
                TabSidebarItem::ResizeHandle => {
                    context.set_cursor(Some(MouseCursor::SizeLeftRight));
                }
                TabSidebarItem::NewTabButton
                | TabSidebarItem::ClosePane { .. }
                | TabSidebarItem::Tab { .. }
                | TabSidebarItem::Pane { .. } => {
                    context.set_cursor(Some(MouseCursor::Hand));
                }
            },
            _ => {}
        }
    }

    fn drag_sidebar_resize(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let mx = event.coords.x as f32;
        let new_width = match self.config.tab_sidebar_position {
            config::TabSidebarPosition::Left => mx as u16,
            config::TabSidebarPosition::Right => {
                (self.dimensions.pixel_width as f32 - mx) as u16
            }
        };
        // Clamp between 120 and 600 pixels
        let new_width = new_width.max(120).min(600);
        if new_width != self.tab_sidebar_width {
            self.tab_sidebar_width = new_width;
            self.invalidate_tab_sidebar();
            self.invalidate_fancy_tab_bar();
            if let Some(window) = self.window.as_ref().map(|w| w.clone()) {
                window.invalidate();
            }
        }
        context.set_cursor(Some(MouseCursor::SizeLeftRight));
        self.dragging.replace((item, start_event));
    }

    fn finish_sidebar_resize(&mut self) {
        if let Some(window) = self.window.as_ref().map(|w| w.clone()) {
            self.apply_dimensions(&self.dimensions.clone(), None, &window);
            window.invalidate();
        }
    }

    /// Returns the sidebar pixel width to subtract from mouse X coordinates
    /// when the sidebar is on the left side.
    pub fn sidebar_x_offset(&self) -> f32 {
        if self.show_tab_sidebar
            && self.config.tab_sidebar_position == config::TabSidebarPosition::Left
        {
            self.tab_sidebar_width as f32
        } else {
            0.
        }
    }

    /// Returns the effective right edge of the terminal area,
    /// accounting for a right-side sidebar.
    pub fn effective_right_edge(&self) -> f32 {
        if self.show_tab_sidebar
            && self.config.tab_sidebar_position == config::TabSidebarPosition::Right
        {
            self.dimensions.pixel_width as f32 - self.tab_sidebar_width as f32
        } else {
            self.dimensions.pixel_width as f32
        }
    }
}

fn compute_drop_zone(
    mouse_x: f32, mouse_y: f32,
    pane_left: f32, pane_top: f32,
    pane_width: f32, pane_height: f32,
) -> DropZone {
    let rx = (mouse_x - pane_left) / pane_width;
    let ry = (mouse_y - pane_top) / pane_height;
    if ry < rx {
        if ry < 1.0 - rx { DropZone::Top } else { DropZone::Right }
    } else {
        if ry < 1.0 - rx { DropZone::Left } else { DropZone::Bottom }
    }
}

fn mouse_press_to_tmb(press: &MousePress) -> TMB {
    match press {
        MousePress::Left => TMB::Left,
        MousePress::Right => TMB::Right,
        MousePress::Middle => TMB::Middle,
    }
}

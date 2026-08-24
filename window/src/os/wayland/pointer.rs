use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use smithay_client_toolkit::compositor::SurfaceData;
use smithay_client_toolkit::reexports::csd_frame::{DecorationsFrame, FrameAction, FrameClick};
use smithay_client_toolkit::seat::pointer::{
    CursorIcon, PointerData, PointerDataExt, PointerEvent, PointerEventKind, PointerHandler,
};
use wayland_client::backend::ObjectId;
use wayland_client::protocol::wl_pointer::{ButtonState, WlPointer};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Proxy, QueueHandle};
use terminaler_input_types::MousePress;

use crate::wayland::SurfaceUserData;

use super::copy_and_paste::CopyAndPaste;
use super::drag_and_drop::DragAndDrop;
use super::state::WaylandState;
use super::WaylandConnection;

impl PointerHandler for WaylandState {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        // The compositor addresses the event to a pointer object; if it isn't
        // one we set up, there is no state to update. Panicking here would
        // abort from inside Wayland event dispatch.
        let Some(udata) = pointer.data::<PointerUserData>() else {
            log::trace!("pointer_frame for a pointer without our user data");
            return;
        };
        let mut pstate = udata.state.lock().unwrap();

        for evt in events {
            if let PointerEventKind::Enter { .. } = &evt.kind {
                let surface_id = evt.surface.id();
                self.active_surface_id = RefCell::new(Some(surface_id.clone()));
                pstate.active_surface_id = Some(surface_id);
            }
            if let Some(serial) = event_serial(&evt) {
                *self.last_serial.borrow_mut() = serial;
                pstate.serial = serial;
            }
            // No Enter has been seen yet if the first event of the session is
            // a Leave, Motion or Axis, so there may be no active surface to
            // attribute this event to.
            let active_surface_id = self.active_surface_id.borrow().clone();
            let Some(active_surface_id) = active_surface_id else {
                continue;
            };
            if let Some(pending) = self.surface_to_pending.get(&active_surface_id) {
                let mut pending = pending.lock().unwrap();
                if pending.queue(evt) {
                    WaylandConnection::with_window_inner(pending.window_id, move |inner| {
                        inner.dispatch_pending_mouse();
                        Ok(())
                    });
                }
            }
        }
        self.pointer_window_frame(_conn, pointer, events);
    }
}

pub(super) struct PointerUserData {
    pub(super) pdata: PointerData,
    pub(super) state: Mutex<PointerState>,
}

impl PointerUserData {
    pub(super) fn new(seat: WlSeat) -> Self {
        Self {
            pdata: PointerData::new(seat),
            state: Default::default(),
        }
    }
}

#[derive(Default)]
pub(super) struct PointerState {
    active_surface_id: Option<ObjectId>,
    pub(super) drag_and_drop: DragAndDrop,
    serial: u32,
}

impl PointerDataExt for PointerUserData {
    fn pointer_data(&self) -> &PointerData {
        &self.pdata
    }
}

#[derive(Clone, Debug)]
pub struct PendingMouse {
    window_id: usize,
    pub(super) copy_and_paste: Arc<Mutex<CopyAndPaste>>,
    surface_coords: Option<(f64, f64)>,
    button: Vec<(MousePress, ButtonState)>,
    scroll: Option<(f64, f64)>,
    in_window: bool,
}

impl PendingMouse {
    pub(super) fn create(
        window_id: usize,
        copy_and_paste: &Arc<Mutex<CopyAndPaste>>,
    ) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            window_id,
            copy_and_paste: Arc::clone(copy_and_paste),
            button: vec![],
            scroll: None,
            surface_coords: None,
            in_window: false,
        }))
    }

    pub(super) fn queue(&mut self, evt: &PointerEvent) -> bool {
        match evt.kind {
            PointerEventKind::Enter { .. } => {
                self.in_window = true;
                false
            }
            PointerEventKind::Leave { .. } => {
                let changed = self.in_window;
                self.surface_coords = None;
                self.in_window = false;
                changed
            }
            PointerEventKind::Motion { .. } => {
                let changed = self.surface_coords.is_none();
                self.surface_coords.replace(evt.position);
                changed
            }
            PointerEventKind::Press { button, .. } | PointerEventKind::Release { button, .. } => {
                fn linux_button(b: u32) -> Option<MousePress> {
                    // See BTN_LEFT and friends in <linux/input-event-codes.h>
                    match b {
                        0x110 => Some(MousePress::Left),
                        0x111 => Some(MousePress::Right),
                        0x112 => Some(MousePress::Middle),
                        _ => None,
                    }
                }
                let button = match linux_button(button) {
                    Some(button) => button,
                    None => return false,
                };
                let changed = self.button.is_empty();
                let button_state = match evt.kind {
                    PointerEventKind::Press { .. } => ButtonState::Pressed,
                    PointerEventKind::Release { .. } => ButtonState::Released,
                    _ => unreachable!(),
                };
                self.button.push((button, button_state));
                changed
            }
            PointerEventKind::Axis {
                horizontal,
                vertical,
                ..
            } => {
                let changed = self.scroll.is_none();
                let (x, y) = self.scroll.take().unwrap_or((0., 0.));
                self.scroll
                    .replace((x + horizontal.absolute, y + vertical.absolute));
                changed
            }
        }
    }

    pub(super) fn next_button(pending: &Arc<Mutex<Self>>) -> Option<(MousePress, ButtonState)> {
        let mut pending = pending.lock().unwrap();
        if pending.button.is_empty() {
            None
        } else {
            Some(pending.button.remove(0))
        }
    }

    pub(super) fn coords(pending: &Arc<Mutex<Self>>) -> Option<(f64, f64)> {
        pending.lock().unwrap().surface_coords.take()
    }

    pub(super) fn scroll(pending: &Arc<Mutex<Self>>) -> Option<(f64, f64)> {
        pending.lock().unwrap().scroll.take()
    }

    pub(super) fn in_window(pending: &Arc<Mutex<Self>>) -> bool {
        pending.lock().unwrap().in_window
    }
}

fn event_serial(event: &PointerEvent) -> Option<u32> {
    Some(match event.kind {
        PointerEventKind::Enter { serial, .. } => serial,
        PointerEventKind::Leave { serial, .. } => serial,
        PointerEventKind::Press { serial, .. } => serial,
        PointerEventKind::Release { serial, .. } => serial,
        _ => return None,
    })
}

impl WaylandState {
    fn pointer_window_frame(
        &mut self,
        conn: &Connection,
        pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        // Collected while the window borrow is held and applied after it is
        // released: setting the cursor needs the connection's wayland_state,
        // which is already mutably borrowed for this dispatch.
        let mut frame_cursor: Option<CursorIcon> = None;
        let windows = self.windows.borrow();

        for evt in events {
            let surface = &evt.surface;
            let active_surface_id = self.active_surface_id.borrow().clone();
            if Some(surface.id()) == active_surface_id {
                let (x, y) = evt.position;
                let parent_surface = match evt.surface.data::<SurfaceData>() {
                    Some(data) => match data.parent_surface() {
                        Some(sd) => sd,
                        None => continue,
                    },
                    None => continue,
                };

                // A parent surface without our user data isn't one of our
                // windows; skip the event rather than panicking out of the
                // event loop. Matches the two `continue`s just above.
                let Some(wid) = SurfaceUserData::try_from_wl(parent_surface).map(|sud| sud.window_id)
                else {
                    continue;
                };
                let Some(window) = windows.get(&wid) else {
                    continue;
                };
                let mut inner = window.borrow_mut();

                match evt.kind {
                    PointerEventKind::Enter { .. } => {
                        // The frame returns the cursor for whatever part of
                        // the decoration the pointer is over — the resize
                        // arrows along the edges and corners in particular.
                        // Dropping it left the plain arrow showing everywhere,
                        // so the window had no visible resize affordance.
                        frame_cursor = inner.window_frame.click_point_moved(
                            Duration::ZERO,
                            &evt.surface.id(),
                            x,
                            y,
                        );
                    }
                    PointerEventKind::Leave { .. } => {
                        inner.window_frame.click_point_left();
                        inner.clear_pending_move();
                    }
                    PointerEventKind::Motion { time } => {
                        frame_cursor = inner.window_frame.click_point_moved(
                            Duration::from_millis(time as u64),
                            &evt.surface.id(),
                            x,
                            y,
                        );
                        // A titlebar press only becomes a window drag once the
                        // pointer travels; until then it might still be the
                        // first half of a double click.
                        inner.maybe_start_pending_move(pointer, x, y);
                    }
                    PointerEventKind::Press {
                        button,
                        serial,
                        time,
                    }
                    | PointerEventKind::Release {
                        button,
                        serial,
                        time,
                    } => {
                        let pressed = if matches!(evt.kind, PointerEventKind::Press { .. }) {
                            true
                        } else {
                            false
                        };
                        let click = match button {
                            0x110 => FrameClick::Normal,
                            0x111 => FrameClick::Alternate,
                            _ => continue,
                        };
                        // The frame compares successive click timestamps to
                        // spot a double click. Passing Duration::ZERO made every
                        // click look like it arrived 0ms after the previous one,
                        // so a single click on the titlebar was treated as a
                        // double click and maximized the window instead of
                        // starting a drag. `time` is the event's own millisecond
                        // clock, which is what the comparison expects.
                        // A release ends the gesture: whatever the press
                        // recorded was a click, not a drag.
                        if !pressed {
                            inner.clear_pending_move();
                        }
                        if let Some(action) = inner.window_frame.on_click(
                            Duration::from_millis(time as u64),
                            click,
                            pressed,
                        ) {
                            // Record where a would-be move started instead of
                            // starting it now; `frame_action` deliberately does
                            // nothing for Move. See `PendingMove`.
                            if matches!(action, FrameAction::Move) {
                                inner.record_pending_move(serial, x, y);
                            }
                            inner.frame_action(pointer, serial, action);
                        }
                    }
                    _ => {}
                }
            }
        }
        drop(windows);

        if let Some(icon) = frame_cursor {
            if let Some(pointer_obj) = &self.pointer {
                if let Err(err) = pointer_obj.set_cursor(conn, icon) {
                    log::error!("set frame cursor: {}", err);
                }
            }
        }
    }
}

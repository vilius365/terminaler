use smithay_client_toolkit::seat::pointer::ThemeSpec;
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, QueueHandle};

use crate::wayland::keyboard::KeyboardData;
use crate::wayland::pointer::PointerUserData;
use crate::wayland::SurfaceUserData;

use super::state::WaylandState;

impl SeatHandler for WaylandState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, seat: WlSeat) {
        // Only fires for a seat appearing after startup — multi-seat setups,
        // or a compositor creating one on input-device hotplug. We track a
        // single seat, so there is no state to add; capabilities on it still
        // arrive through new_capability.
        log::warn!("additional seat {seat:?} is not tracked");
    }

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard if self.keyboard.is_none() => {
                log::trace!("Setting keyboard capability");
                let keyboard = seat.get_keyboard(qh, KeyboardData {});
                self.keyboard = Some(keyboard.clone());

                if let Some(text_input) = &self.text_input {
                    text_input.advise_seat(&seat, &keyboard, qh);
                }
            }
            Capability::Pointer if self.pointer.is_none() => {
                log::trace!("Setting pointer capability");
                let surface = self.compositor.create_surface(qh);
                let pointer = self
                    .seat
                    .get_pointer_with_theme_and_data::<WaylandState, SurfaceUserData, PointerUserData>(
                        qh,
                        &seat,
                        &self.shm.wl_shm(),
                        surface,
                        ThemeSpec::System,
                        PointerUserData::new(seat.clone()),
                    )
                    .expect("Failed to create pointer");
                self.pointer = Some(pointer);
            }
            Capability::Touch /* if self.touch.is_none() */ => {
                log::trace!("Setting touch capability");
                // TODO
            }
            _ => {}
        }

        // TODO: is there a better place to put this? It only needs to be run once. (presumably per-seat)
        if self.data_device.is_none() {
            let data_device_manager = &self.data_device_manager_state;
            self.data_device = Some(data_device_manager.get_data_device(qh, &seat));

            self.primary_selection_device = self
                .primary_selection_manager
                .as_ref()
                .map(|m| m.get_selection_device(qh, &seat));
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard => {
                log::trace!("Lost keyboard capability");
                self.keyboard.take().map(|k| k.release());
            }
            Capability::Pointer => {
                log::trace!("Lost pointer capability");
                self.pointer.take(); // ThemedPointer's drop implementation calls wl_pointer.release() already.
            }
            Capability::Touch => {
                log::trace!("Lost touch capability");
                // Nothing to do here. (yet)
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, seat: WlSeat) {
        // The compositor withdraws a seat's capabilities before removing the
        // seat itself, so remove_capability has already released the keyboard
        // and pointer by this point.
        log::trace!("seat {seat:?} removed");
    }
}

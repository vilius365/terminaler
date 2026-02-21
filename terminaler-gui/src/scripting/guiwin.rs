//! GuiWin represents a Gui TermWindow (as opposed to a Mux window).
//! Lua/mlua UserData implementations have been removed (Phase 1).
use crate::TermWindow;
use mux::pane::PaneId;
use mux::tab::TabId;
use mux::window::WindowId as MuxWindowId;

/// Minimal stub for MuxPane (was mux_lua::MuxPane).
/// Wraps a PaneId for passing between GUI and Rust.
#[derive(Clone, Copy)]
pub struct MuxPane(pub PaneId);

/// Minimal stub for MuxWindow (was mux_lua::MuxWindow).
#[derive(Clone, Copy)]
pub struct MuxWindow(pub MuxWindowId);

/// Minimal stub for MuxTab (was mux_lua::MuxTab).
#[derive(Clone, Copy)]
pub struct MuxTab(pub TabId);

/// Minimal stub for MuxDomain (was mux_lua::MuxDomain).
/// Wraps a DomainId for passing between GUI and Rust.
#[derive(Clone, Copy)]
pub struct MuxDomain(pub mux::domain::DomainId);

#[derive(Clone)]
pub struct GuiWin {
    pub mux_window_id: MuxWindowId,
    pub window: ::window::Window,
}

impl GuiWin {
    pub fn new(term_window: &TermWindow) -> Self {
        let window = term_window.window.clone().unwrap();
        let mux_window_id = term_window.mux_window_id;
        Self {
            window,
            mux_window_id,
        }
    }
}

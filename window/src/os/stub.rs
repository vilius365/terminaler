//! Stub platform implementation for non-Windows builds.
//! The terminaler-gui binary is Windows-only.
//! These stubs allow the crate to compile on Linux/CI without
//! a real windowing backend.

use async_trait::async_trait;
use crate::connection::ConnectionOps;
use crate::screen::Screens;
use crate::{
    Clipboard, MouseCursor, ResizeIncrement,
    ScreenPoint, WindowOps, WindowState,
};
use anyhow::Result as Fallible;
use promise::Future;
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use std::any::Any;
use std::rc::Rc;

/// Stub Connection type for non-Windows platforms.
pub struct Connection;

impl Connection {
    pub fn get() -> Option<Rc<Self>> {
        None
    }

    pub fn init() -> Fallible<Rc<Self>> {
        anyhow::bail!("Connection::init() is not supported on this platform")
    }
}

impl ConnectionOps for Connection {
    fn name(&self) -> String {
        "stub".to_string()
    }

    fn terminate_message_loop(&self) {}

    fn run_message_loop(&self) -> Fallible<()> {
        anyhow::bail!("run_message_loop() is not supported on this platform")
    }

    fn screens(&self) -> anyhow::Result<Screens> {
        anyhow::bail!("screens() is not supported on this platform")
    }
}

/// Stub Window type for non-Windows platforms.
#[derive(Clone)]
pub struct Window;

impl Window {
    pub async fn new_window<F>(
        _class_name: &str,
        _name: &str,
        _geometry: crate::RequestedWindowGeometry,
        _config: Option<&config::ConfigHandle>,
        _font_config: std::rc::Rc<terminaler_font::FontConfiguration>,
        _event_handler: F,
    ) -> anyhow::Result<Window>
    where
        F: 'static + FnMut(crate::WindowEvent, &Window),
    {
        anyhow::bail!("Window creation is not supported on this platform")
    }
}

impl std::fmt::Debug for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Window(stub)")
    }
}

impl PartialEq for Window {
    fn eq(&self, _other: &Self) -> bool {
        false
    }
}

impl Eq for Window {}

impl std::hash::Hash for Window {
    fn hash<H: std::hash::Hasher>(&self, _state: &mut H) {}
}

impl PartialOrd for Window {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Window {
    fn cmp(&self, _other: &Self) -> std::cmp::Ordering {
        std::cmp::Ordering::Equal
    }
}

impl HasWindowHandle for Window {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Err(HandleError::NotSupported)
    }
}

impl HasDisplayHandle for Window {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Err(HandleError::NotSupported)
    }
}

#[async_trait(?Send)]
impl WindowOps for Window {
    fn show(&self) {}

    fn notify<T: Any + Send + Sync>(&self, _t: T)
    where
        Self: Sized,
    {
    }

    async fn enable_opengl(&self) -> anyhow::Result<Rc<glium::backend::Context>> {
        anyhow::bail!("enable_opengl() is not supported on this platform")
    }

    fn hide(&self) {}
    fn toggle_fullscreen(&self) {}
    fn maximize(&self) {}
    fn restore(&self) {}
    fn focus(&self) {}
    fn close(&self) {}

    fn set_cursor(&self, _cursor: Option<MouseCursor>) {}
    fn invalidate(&self) {}
    fn set_title(&self, _title: &str) {}

    fn get_clipboard(&self, _clipboard: Clipboard) -> Future<String> {
        Future::err(anyhow::anyhow!("clipboard not supported"))
    }

    fn set_clipboard(&self, _clipboard: Clipboard, _text: String) {}
    fn set_window_position(&self, _coords: ScreenPoint) {}
    fn set_inner_size(&self, _width: usize, _height: usize) {}
    fn request_drag_move(&self) {}
    fn set_resize_increments(&self, _incr: ResizeIncrement) {}

    fn get_os_parameters(
        &self,
        _config: &config::ConfigHandle,
        _window_state: WindowState,
    ) -> anyhow::Result<Option<crate::os::parameters::Parameters>> {
        Ok(None)
    }
}

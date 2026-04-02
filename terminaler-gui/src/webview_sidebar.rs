//! WebView2-based sidebar panel.
//!
//! Embeds a WebView2 child window inside the main terminal window to render
//! the tab sidebar using HTML/CSS, achieving full design mockup fidelity.
//! Falls back to the GPU-rendered sidebar if WebView2 is unavailable.

#[cfg(windows)]
mod inner {
    use anyhow::Context;
    use std::hash::{Hash, Hasher};
    use std::sync::{Arc, Mutex};
    use wry::raw_window_handle::{RawWindowHandle, Win32WindowHandle, WindowHandle};
    use wry::{Rect, WebView, WebViewBuilder};

    /// Post a WM_APP message to the parent window to defer SetFocus.
    /// This avoids the RefCell re-entrancy crash that happens when SetFocus
    /// is called synchronously (it dispatches WM_SETFOCUS immediately,
    /// which tries to borrow the WindowInner that's already borrowed).
    const WM_APP_REFOCUS: u32 = winapi::um::winuser::WM_APP + 100;

    /// Request the parent window to take focus, deferred via PostMessage.
    unsafe fn post_refocus(hwnd: isize) {
        winapi::um::winuser::PostMessageW(
            hwnd as winapi::shared::windef::HWND,
            WM_APP_REFOCUS,
            0,
            0,
        );
    }

    /// The sidebar HTML, embedded at compile time.
    const SIDEBAR_HTML: &str = include_str!("../assets/sidebar.html");

    /// Thread-safe queue for IPC messages from the WebView.
    /// The IPC handler pushes messages here; the paint loop drains them.
    pub type IpcQueue = Arc<Mutex<Vec<String>>>;

    pub struct WebViewSidebar {
        webview: WebView,
        /// Parent HWND, used to refocus after WebView interactions.
        parent_hwnd: isize,
        /// Pending IPC messages from JS, drained each paint frame.
        pub ipc_queue: IpcQueue,
        /// Last state JSON hash, for change detection.
        last_state_hash: u64,
        /// Last bounds applied, to avoid redundant set_bounds calls.
        last_bounds: (i32, i32, u16, u16),
    }

    impl WebViewSidebar {
        /// Create a new WebView2 child window inside the given parent HWND.
        /// IPC messages from JS are queued in `ipc_queue` — the caller must
        /// drain them from the paint loop (NOT from a Win32 message handler).
        pub fn new(
            parent_hwnd: isize,
            x: i32,
            y: i32,
            width: u32,
            height: u32,
        ) -> anyhow::Result<Self> {
            use std::num::NonZeroIsize;

            log::info!("WebViewSidebar::new() start hwnd={} bounds=({},{},{}x{})",
                parent_hwnd, x, y, width, height);

            let win_handle =
                Win32WindowHandle::new(NonZeroIsize::new(parent_hwnd).context("null HWND")?);

            let parent =
                unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(win_handle)) };

            let bounds = Rect {
                position: wry::dpi::Position::Physical(wry::dpi::PhysicalPosition::new(x, y)),
                size: wry::dpi::Size::Physical(wry::dpi::PhysicalSize::new(width, height)),
            };

            let queue: IpcQueue = Arc::new(Mutex::new(Vec::new()));
            let queue_clone = Arc::clone(&queue);

            log::info!("WebViewSidebar: building WebView...");

            let webview = WebViewBuilder::new()
                .with_bounds(bounds)
                .with_transparent(false)
                .with_focused(false)
                .with_ipc_handler(move |msg: wry::http::Request<String>| {
                    log::trace!("WebViewSidebar IPC queued: {}", msg.body());
                    if let Ok(mut q) = queue_clone.lock() {
                        q.push(msg.body().to_string());
                    }
                })
                .with_html(SIDEBAR_HTML)
                .build_as_child(&parent)
                .context("creating WebView2 sidebar")?;

            log::info!("WebViewSidebar: WebView created successfully");

            Ok(Self {
                webview,
                parent_hwnd,
                ipc_queue: queue,
                last_state_hash: 0,
                last_bounds: (x, y, width as u16, height as u16),
            })
        }

        /// Request keyboard focus return to the parent terminal window.
        /// Uses PostMessage to defer — safe to call from paint loop.
        pub fn refocus_parent(&self) {
            log::trace!("WebViewSidebar: refocus_parent (deferred)");
            unsafe { post_refocus(self.parent_hwnd); }
        }

        /// The custom message ID used for deferred refocus.
        pub fn refocus_message_id() -> u32 {
            WM_APP_REFOCUS
        }

        /// Reposition/resize the WebView to match new sidebar geometry.
        /// Only calls set_bounds if the geometry actually changed.
        pub fn reposition(&mut self, x: i32, y: i32, width: u16, height: u16) {
            let new_bounds = (x, y, width, height);
            if new_bounds == self.last_bounds {
                return;
            }
            log::debug!("WebViewSidebar: reposition ({},{},{}x{}) -> ({},{},{}x{})",
                self.last_bounds.0, self.last_bounds.1, self.last_bounds.2, self.last_bounds.3,
                x, y, width, height);
            self.last_bounds = new_bounds;

            let bounds = Rect {
                position: wry::dpi::Position::Physical(wry::dpi::PhysicalPosition::new(x, y)),
                size: wry::dpi::Size::Physical(wry::dpi::PhysicalSize::new(
                    width as u32,
                    height as u32,
                )),
            };
            if let Err(e) = self.webview.set_bounds(bounds) {
                log::warn!("WebViewSidebar: set_bounds failed: {:#}", e);
            }
            log::debug!("WebViewSidebar: reposition done");
        }

        /// Show or hide the WebView child window.
        pub fn set_visible(&self, visible: bool) {
            log::debug!("WebViewSidebar: set_visible({})", visible);
            if let Err(e) = self.webview.set_visible(visible) {
                log::warn!("WebViewSidebar: set_visible failed: {:#}", e);
            }
        }

        /// Push a JSON state object to the sidebar JS.
        /// Only calls evaluate_script if the state hash changed.
        pub fn push_state(&mut self, json: &str) {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            json.hash(&mut hasher);
            let hash = hasher.finish();

            if hash == self.last_state_hash {
                return;
            }
            self.last_state_hash = hash;

            log::trace!("WebViewSidebar: push_state ({}B)", json.len());
            let script = format!("window.__updateState({})", json);
            if let Err(e) = self.webview.evaluate_script(&script) {
                log::warn!("WebViewSidebar: evaluate_script failed: {:#}", e);
            }
        }
    }
}

#[cfg(windows)]
pub use inner::WebViewSidebar;

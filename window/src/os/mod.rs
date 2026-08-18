#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(all(unix, not(target_os = "macos")))]
#[cfg(feature = "wayland")]
pub mod wayland;
#[cfg(all(unix, not(target_os = "macos")))]
pub mod x11;
#[cfg(all(unix, not(target_os = "macos")))]
pub mod x_and_wayland;
#[cfg(all(unix, not(target_os = "macos")))]
pub mod xdg_desktop_portal;
#[cfg(all(unix, not(target_os = "macos")))]
pub mod xkeysyms;

#[cfg(all(unix, not(target_os = "macos")))]
pub use x_and_wayland::*;

// Fallback stub for platforms with no real backend (e.g. macOS, which this
// fork does not support).
#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
pub mod stub;
#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
pub use self::stub::*;

pub mod parameters;

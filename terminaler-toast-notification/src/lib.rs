// STRIPPED: mod dbus;   -- Linux D-Bus notifications removed (Windows-only)
// STRIPPED: mod macos;  -- macOS notifications removed (Windows-only)
pub mod slack;
mod windows;

#[derive(Debug, Clone)]
pub struct ToastNotification {
    pub title: String,
    pub message: String,
    pub url: Option<String>,
    pub timeout: Option<std::time::Duration>,
}

impl ToastNotification {
    pub fn show(self) {
        show(self)
    }
}

#[cfg(windows)]
use crate::windows as backend;
#[cfg(not(windows))]
mod nop {
    use super::*;

    pub fn show_notif(_: ToastNotification) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}
#[cfg(not(windows))]
use nop as backend;

pub fn show(notif: ToastNotification) {
    if let Err(err) = backend::show_notif(notif) {
        log::error!("Failed to show notification: {}", err);
    }
}

pub fn persistent_toast_notification_with_click_to_open_url(title: &str, message: &str, _url: &str) {
    show(ToastNotification {
        title: title.to_string(),
        message: message.to_string(),
        url: Some(_url.to_string()),
        timeout: None,
    });
}

pub fn persistent_toast_notification(title: &str, message: &str) {
    show(ToastNotification {
        title: title.to_string(),
        message: message.to_string(),
        url: None,
        timeout: None,
    });
}

/// Show a toast notification and, if a Slack webhook URL is provided, send a Slack message.
/// The Slack POST is fire-and-forget on a background thread.
pub fn notify_all(title: &str, message: &str, slack_webhook: Option<&str>) {
    persistent_toast_notification(title, message);
    if let Some(webhook_url) = slack_webhook {
        if !webhook_url.is_empty() {
            slack::send_notification(webhook_url, title, message);
        }
    }
}

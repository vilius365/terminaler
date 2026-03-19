use std::convert::TryFrom;
use std::thread;

/// Send a Slack notification on a background thread (fire-and-forget).
pub fn send_notification(webhook_url: &str, title: &str, message: &str) {
    let webhook_url = webhook_url.to_string();
    let title = title.to_string();
    let message = message.to_string();

    thread::spawn(move || {
        send_notification_sync(&webhook_url, &title, &message);
    });
}

/// Send a Slack notification synchronously (blocking). Call from a background thread.
pub fn send_notification_sync(webhook_url: &str, title: &str, message: &str) {
    if let Err(err) = post_to_slack(webhook_url, title, message) {
        log::error!("Failed to send Slack notification: {}", err);
    }
}

fn post_to_slack(
    webhook_url: &str,
    title: &str,
    message: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let body = serde_json::json!({
        "blocks": [
            {
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!("*{}*\n{}", title, message)
                }
            }
        ]
    });

    let body_bytes = serde_json::to_vec(&body)?;

    let uri = http_req::uri::Uri::try_from(webhook_url.as_ref())?;

    let mut response_body = Vec::new();
    let response = http_req::request::Request::new(&uri)
        .method(http_req::request::Method::POST)
        .header("Content-Type", "application/json")
        .header("Content-Length", &body_bytes.len())
        .body(&body_bytes)
        .send(&mut response_body)?;

    let status = response.status_code();
    if !status.is_success() {
        let body_str = String::from_utf8_lossy(&response_body);
        return Err(format!("Slack webhook returned HTTP {}: {}", status, body_str).into());
    }

    Ok(())
}

use crate::ansi_render;
use crate::bridge::MuxBridge;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use mux::pane::{Pane, PaneId};
use mux::{Mux, MuxNotification};
use std::sync::Arc;
use termwiz::surface::SequenceNo;
use tokio::sync::broadcast;

/// Handle a single WebSocket connection.
pub async fn handle_ws(socket: WebSocket, bridge: Arc<MuxBridge>) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut broadcast_rx = bridge.broadcast_tx.subscribe();

    // Current attached pane state
    let mut attached_pane_id: Option<PaneId> = None;
    let mut last_seqno: SequenceNo = 0;

    loop {
        tokio::select! {
            // Messages from the browser client
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Text(ref text))) => {
                        let text_str: &str = text.as_ref();
                        if let Err(e) = handle_client_message(
                            text_str,
                            &mut ws_tx,
                            &mut attached_pane_id,
                            &mut last_seqno,
                        ).await {
                            log::debug!("WebSocket client message error: {:#}", e);
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }

            // Mux notifications (output changes, pane add/remove)
            notification = broadcast_rx.recv() => {
                match notification {
                    Ok(MuxNotification::PaneOutput(pane_id)) => {
                        if attached_pane_id == Some(pane_id) {
                            if let Err(e) = send_delta_output(
                                &mut ws_tx, pane_id, &mut last_seqno
                            ).await {
                                log::debug!("Failed to send delta output: {:#}", e);
                                break;
                            }
                        }
                    }
                    Ok(MuxNotification::PaneAdded(_)) => {
                        if let Ok(list) = build_pane_list().await {
                            let msg = serde_json::json!({"type": "pane_list", "panes": list});
                            let text: String = msg.to_string();
                            if ws_tx.send(Message::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Ok(MuxNotification::PaneRemoved(id)) => {
                        // Notify client of pane list change
                        if let Ok(list) = build_pane_list().await {
                            let msg = serde_json::json!({"type": "pane_list", "panes": list});
                            let text: String = msg.to_string();
                            if ws_tx.send(Message::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                        // If our attached pane was removed, detach
                        if attached_pane_id == Some(id) {
                            attached_pane_id = None;
                            let msg = serde_json::json!({"type": "pane_removed", "pane_id": id});
                            let text: String = msg.to_string();
                            if ws_tx.send(Message::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        log::debug!("WebSocket broadcast lagged by {n} messages");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    _ => {}
                }
            }
        }
    }
}

type WsSink = futures_util::stream::SplitSink<WebSocket, Message>;

/// Process a client JSON message.
async fn handle_client_message(
    text: &str,
    ws_tx: &mut WsSink,
    attached_pane_id: &mut Option<PaneId>,
    last_seqno: &mut SequenceNo,
) -> anyhow::Result<()> {
    let msg: serde_json::Value = serde_json::from_str(text)?;
    let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match msg_type {
        "list_panes" => {
            let list = build_pane_list().await?;
            let resp = serde_json::json!({"type": "pane_list", "panes": list});
            let text: String = resp.to_string();
            ws_tx.send(Message::Text(text.into())).await?;
        }
        "attach" => {
            let pane_id = msg
                .get("pane_id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("missing pane_id"))? as PaneId;

            *attached_pane_id = Some(pane_id);
            *last_seqno = 0;

            // Send full screen refresh
            send_full_refresh(ws_tx, pane_id, last_seqno).await?;
        }
        "input" => {
            let pane_id = msg
                .get("pane_id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("missing pane_id"))? as PaneId;
            let data = msg
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            send_input_to_pane(pane_id, data.to_string()).await?;
        }
        "paste" => {
            let pane_id = msg
                .get("pane_id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("missing pane_id"))? as PaneId;
            let data = msg
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            send_paste_to_pane(pane_id, data.to_string()).await?;
        }
        "resize" => {
            let pane_id = msg
                .get("pane_id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("missing pane_id"))? as PaneId;
            let cols = msg.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as usize;
            let rows = msg.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as usize;

            resize_pane(pane_id, cols, rows).await?;
        }
        _ => {
            log::debug!("Unknown WebSocket message type: {}", msg_type);
        }
    }
    Ok(())
}

/// Build the pane list by dispatching to the smol main thread.
async fn build_pane_list() -> anyhow::Result<Vec<serde_json::Value>> {
    let result = promise::spawn::spawn_into_main_thread(async {
        let mux = Mux::get();
        let panes = mux.iter_panes();
        panes
            .iter()
            .map(|pane| {
                let dims = pane.get_dimensions();
                serde_json::json!({
                    "id": pane.pane_id(),
                    "title": pane.get_title(),
                    "cols": dims.cols,
                    "rows": dims.viewport_rows,
                })
            })
            .collect::<Vec<_>>()
    })
    .await;
    Ok(result)
}

/// Send a full screen refresh for the given pane.
async fn send_full_refresh(
    ws_tx: &mut WsSink,
    pane_id: PaneId,
    last_seqno: &mut SequenceNo,
) -> anyhow::Result<()> {
    let (ansi_data, seqno, cols, rows) =
        promise::spawn::spawn_into_main_thread(async move {
            let mux = Mux::get();
            if let Some(pane) = mux.get_pane(pane_id) {
                let dims = pane.get_dimensions();
                let viewport_range = dims.physical_top
                    ..dims.physical_top + dims.viewport_rows as isize;
                let (_, lines) = pane.get_lines(viewport_range);

                let seqno = lines
                    .last()
                    .map(|l| l.current_seqno())
                    .unwrap_or(0);

                let ansi = ansi_render::full_screen_ansi(&lines, dims.cols, dims.viewport_rows);
                (ansi, seqno, dims.cols, dims.viewport_rows)
            } else {
                (String::new(), 0, 80, 24)
            }
        })
        .await;

    *last_seqno = seqno;

    let msg = serde_json::json!({
        "type": "output",
        "pane_id": pane_id,
        "data": ansi_data,
        "cols": cols,
        "rows": rows,
    });
    let text: String = msg.to_string();
    ws_tx.send(Message::Text(text.into())).await?;
    Ok(())
}

/// Send delta output for changed lines since last seqno.
async fn send_delta_output(
    ws_tx: &mut WsSink,
    pane_id: PaneId,
    last_seqno: &mut SequenceNo,
) -> anyhow::Result<()> {
    let prev_seqno = *last_seqno;
    let (ansi_data, new_seqno) =
        promise::spawn::spawn_into_main_thread(async move {
            let mux = Mux::get();
            if let Some(pane) = mux.get_pane(pane_id) {
                let dims = pane.get_dimensions();
                let viewport_range = dims.physical_top
                    ..dims.physical_top + dims.viewport_rows as isize;

                let changed = pane.get_changed_since(viewport_range.clone(), prev_seqno);

                if changed.is_empty() {
                    return (String::new(), prev_seqno);
                }

                // Get the changed lines and render them
                let mut ansi = String::new();
                for range in changed.iter() {
                    let (first_row, lines) = pane.get_lines(range.clone());
                    let row_offset = (first_row - dims.physical_top) as usize + 1;
                    ansi.push_str(&ansi_render::lines_to_ansi(&lines, row_offset));
                }

                let new_seqno = get_max_seqno(&pane, &dims);
                (ansi, new_seqno)
            } else {
                (String::new(), prev_seqno)
            }
        })
        .await;

    if !ansi_data.is_empty() {
        *last_seqno = new_seqno;
        let msg = serde_json::json!({
            "type": "output",
            "pane_id": pane_id,
            "data": ansi_data,
        });
        let text: String = msg.to_string();
        ws_tx.send(Message::Text(text.into())).await?;
    }
    Ok(())
}

/// Get the max sequence number from current viewport lines.
fn get_max_seqno(
    pane: &Arc<dyn Pane>,
    dims: &mux::renderable::RenderableDimensions,
) -> SequenceNo {
    let viewport_range = dims.physical_top..dims.physical_top + dims.viewport_rows as isize;
    let (_, lines) = pane.get_lines(viewport_range);
    lines
        .iter()
        .map(|l| l.current_seqno())
        .max()
        .unwrap_or(0)
}

/// Send keyboard input to a pane via the smol main thread.
async fn send_input_to_pane(pane_id: PaneId, data: String) -> anyhow::Result<()> {
    promise::spawn::spawn_into_main_thread(async move {
        let mux = Mux::get();
        if let Some(pane) = mux.get_pane(pane_id) {
            if let Err(e) = pane.writer().write_all(data.as_bytes()) {
                log::error!("Failed to write to pane {}: {:#}", pane_id, e);
            }
        }
    })
    .await;
    Ok(())
}

/// Send paste text to a pane.
async fn send_paste_to_pane(pane_id: PaneId, data: String) -> anyhow::Result<()> {
    promise::spawn::spawn_into_main_thread(async move {
        let mux = Mux::get();
        if let Some(pane) = mux.get_pane(pane_id) {
            if let Err(e) = pane.send_paste(&data) {
                log::error!("Failed to paste to pane {}: {:#}", pane_id, e);
            }
        }
    })
    .await;
    Ok(())
}

/// Resize a pane via the smol main thread.
async fn resize_pane(pane_id: PaneId, cols: usize, rows: usize) -> anyhow::Result<()> {
    promise::spawn::spawn_into_main_thread(async move {
        let mux = Mux::get();
        if let Some(pane) = mux.get_pane(pane_id) {
            let size = terminaler_term::TerminalSize {
                rows,
                cols,
                pixel_width: cols * 8,
                pixel_height: rows * 16,
                dpi: 96,
            };
            if let Err(e) = pane.resize(size) {
                log::error!("Failed to resize pane {}: {:#}", pane_id, e);
            }
        }
    })
    .await;
    Ok(())
}

use crate::ansi_render;
use crate::bridge::MuxBridge;
use crate::json_render;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use mux::pane::{Pane, PaneId};
use mux::{Mux, MuxNotification};
use std::sync::Arc;
use terminaler_term::TerminalSize;
use termwiz::surface::SequenceNo;
use tokio::sync::broadcast;

/// Output format negotiated per WebSocket session.
#[derive(Debug, Clone, Copy, PartialEq)]
enum OutputFormat {
    Ansi,
    Json,
}

/// Handle a single WebSocket connection.
pub async fn handle_ws(socket: WebSocket, bridge: Arc<MuxBridge>) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut broadcast_rx = bridge.broadcast_tx.subscribe();

    // Current attached pane state
    let mut attached_pane_id: Option<PaneId> = None;
    let mut last_seqno: SequenceNo = 0;
    let mut output_format = OutputFormat::Ansi;
    // Track viewport top for scrollback detection in JSON mode
    let mut last_physical_top: isize = 0;

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
                            &mut last_physical_top,
                            &mut output_format,
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
                            if let Err(e) = send_delta(
                                &mut ws_tx, pane_id, &mut last_seqno, &mut last_physical_top, output_format
                            ).await {
                                log::debug!("Failed to send delta output: {:#}", e);
                                break;
                            }
                        }
                    }
                    Ok(MuxNotification::TabResized(_tab_id)) => {
                        // When the native app resizes, pane dimensions change.
                        // Send a full refresh so the web client repaints cleanly.
                        if let Some(pane_id) = attached_pane_id {
                            if let Err(e) = send_refresh(
                                &mut ws_tx, pane_id, &mut last_seqno, &mut last_physical_top, output_format
                            ).await {
                                log::debug!("Failed to send refresh on resize: {:#}", e);
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

/// Process a client JSON message.  Returns true if a resize was handled and
/// the caller should send a full refresh.
async fn handle_client_message(
    text: &str,
    ws_tx: &mut WsSink,
    attached_pane_id: &mut Option<PaneId>,
    last_seqno: &mut SequenceNo,
    last_physical_top: &mut isize,
    output_format: &mut OutputFormat,
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

            // Parse optional format field
            let format_str = msg.get("format").and_then(|v| v.as_str()).unwrap_or("ansi");
            *output_format = match format_str {
                "json" => OutputFormat::Json,
                _ => OutputFormat::Ansi,
            };

            *attached_pane_id = Some(pane_id);
            *last_seqno = 0;

            // Send full screen refresh in the negotiated format
            send_refresh(ws_tx, pane_id, last_seqno, last_physical_top, *output_format).await?;
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
            let cols = msg
                .get("cols")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("missing cols"))? as usize;
            let rows = msg
                .get("rows")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("missing rows"))? as usize;

            resize_pane(pane_id, cols, rows).await?;

            // Send full refresh after resize
            if let Some(pid) = *attached_pane_id {
                if pid == pane_id {
                    *last_seqno = 0;
                    send_refresh(ws_tx, pane_id, last_seqno, last_physical_top, *output_format).await?;
                }
            }
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

/// Format-aware full refresh dispatcher.
async fn send_refresh(
    ws_tx: &mut WsSink,
    pane_id: PaneId,
    last_seqno: &mut SequenceNo,
    last_physical_top: &mut isize,
    format: OutputFormat,
) -> anyhow::Result<()> {
    match format {
        OutputFormat::Ansi => send_full_refresh(ws_tx, pane_id, last_seqno).await,
        OutputFormat::Json => send_full_refresh_json(ws_tx, pane_id, last_seqno, last_physical_top).await,
    }
}

/// Format-aware delta update dispatcher.
async fn send_delta(
    ws_tx: &mut WsSink,
    pane_id: PaneId,
    last_seqno: &mut SequenceNo,
    last_physical_top: &mut isize,
    format: OutputFormat,
) -> anyhow::Result<()> {
    match format {
        OutputFormat::Ansi => send_delta_output(ws_tx, pane_id, last_seqno).await,
        OutputFormat::Json => send_delta_output_json(ws_tx, pane_id, last_seqno, last_physical_top).await,
    }
}

/// Send a full screen refresh for the given pane, including scrollback history.
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

                // Include up to 1000 scrollback lines above the viewport
                let scrollback_limit: isize = 1000;
                let scrollback_start =
                    (dims.physical_top - scrollback_limit).max(dims.scrollback_top);
                let full_range =
                    scrollback_start..dims.physical_top + dims.viewport_rows as isize;
                let (first_row, all_lines) = pane.get_lines(full_range);

                let seqno = all_lines
                    .iter()
                    .map(|l| l.current_seqno())
                    .max()
                    .unwrap_or(0);

                // Split into scrollback and viewport portions
                let viewport_start_idx =
                    (dims.physical_top - first_row) as usize;
                let viewport_start_idx = viewport_start_idx.min(all_lines.len());
                let scrollback_lines = &all_lines[..viewport_start_idx];
                let viewport_lines = &all_lines[viewport_start_idx..];

                // Get cursor position (1-based, viewport-relative)
                let cursor = pane.get_cursor_position();
                let cursor_row =
                    ((cursor.y - dims.physical_top) as usize + 1).clamp(1, dims.viewport_rows);
                let cursor_col = cursor.x.max(1);

                let ansi = ansi_render::full_refresh_with_scrollback(
                    scrollback_lines,
                    viewport_lines,
                    dims.cols,
                    dims.viewport_rows,
                    cursor_row,
                    cursor_col,
                );
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

                // Position cursor at its actual location so it blinks in the right place
                let cursor = pane.get_cursor_position();
                let cursor_row =
                    ((cursor.y - dims.physical_top) as usize + 1).clamp(1, dims.viewport_rows);
                let cursor_col = cursor.x.max(1);
                use std::fmt::Write;
                write!(ansi, "\x1b[{};{}H", cursor_row, cursor_col).unwrap();

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

/// Send a full screen refresh in JSON span format.
async fn send_full_refresh_json(
    ws_tx: &mut WsSink,
    pane_id: PaneId,
    last_seqno: &mut SequenceNo,
    last_physical_top: &mut isize,
) -> anyhow::Result<()> {
    let data = promise::spawn::spawn_into_main_thread(async move {
        let mux = Mux::get();
        if let Some(pane) = mux.get_pane(pane_id) {
            let dims = pane.get_dimensions();
            log::info!(
                "web full_refresh: physical_top={} viewport_rows={} scrollback_top={} cols={}",
                dims.physical_top, dims.viewport_rows, dims.scrollback_top, dims.cols
            );

            // Include up to 1000 scrollback lines above the viewport
            let scrollback_limit: isize = 1000;
            let scrollback_start =
                (dims.physical_top - scrollback_limit).max(dims.scrollback_top);
            let full_range =
                scrollback_start..dims.physical_top + dims.viewport_rows as isize;
            let (first_row, all_lines) = pane.get_lines(full_range);

            let seqno = all_lines
                .iter()
                .map(|l| l.current_seqno())
                .max()
                .unwrap_or(0);

            // Split into scrollback and viewport portions
            let viewport_start_idx =
                (dims.physical_top - first_row) as usize;
            let viewport_start_idx = viewport_start_idx.min(all_lines.len());
            let scrollback_lines = &all_lines[..viewport_start_idx];
            let viewport_end_idx = (viewport_start_idx + dims.viewport_rows).min(all_lines.len());
            let viewport_lines = &all_lines[viewport_start_idx..viewport_end_idx];

            log::info!(
                "web full_refresh: first_row={} all_lines={} vp_start_idx={} vp_end_idx={} vp_lines={} sb_lines={}",
                first_row, all_lines.len(), viewport_start_idx, viewport_end_idx, viewport_lines.len(), scrollback_lines.len()
            );

            let scrollback_spans = json_render::lines_to_spans(scrollback_lines);
            let viewport_spans = json_render::lines_to_spans(viewport_lines);

            // Get cursor position (0-based, viewport-relative)
            let cursor = pane.get_cursor_position();
            let cursor_y = ((cursor.y - dims.physical_top) as usize).min(dims.viewport_rows.saturating_sub(1));
            let cursor_info = json_render::CursorInfo {
                x: cursor.x,
                y: cursor_y,
                shape: json_render::cursor_shape_to_string(cursor.shape).to_string(),
                visible: cursor.visibility == termwiz::surface::CursorVisibility::Visible,
            };

            Some((scrollback_spans, viewport_spans, cursor_info, seqno, dims.cols, dims.viewport_rows, dims.physical_top, first_row, all_lines.len(), viewport_start_idx, viewport_end_idx))
        } else {
            None
        }
    })
    .await;

    if let Some((scrollback_spans, viewport_spans, cursor_info, seqno, cols, rows, physical_top, first_row, all_lines_len, vp_start_idx, vp_end_idx)) = data {
        *last_seqno = seqno;
        *last_physical_top = physical_top;
        let msg = serde_json::json!({
            "type": "screen",
            "pane_id": pane_id,
            "scrollback": scrollback_spans,
            "viewport": viewport_spans,
            "cursor": cursor_info,
            "cols": cols,
            "rows": rows,
            "_debug": {
                "physical_top": physical_top,
                "first_row": first_row,
                "all_lines": all_lines_len,
                "vp_start_idx": vp_start_idx,
                "vp_end_idx": vp_end_idx,
                "viewport_rows": rows,
                "scrollback_sent": vp_start_idx,
            }
        });
        let text: String = msg.to_string();
        ws_tx.send(Message::Text(text.into())).await?;
    }
    Ok(())
}

/// Send delta output in JSON span format for changed lines since last seqno.
/// Also detects when content has scrolled off the viewport into scrollback
/// and includes those lines as `scrollback_append` so the client can maintain
/// a complete scrollback history.
async fn send_delta_output_json(
    ws_tx: &mut WsSink,
    pane_id: PaneId,
    last_seqno: &mut SequenceNo,
    last_physical_top: &mut isize,
) -> anyhow::Result<()> {
    let prev_seqno = *last_seqno;
    let prev_physical_top = *last_physical_top;
    let data = promise::spawn::spawn_into_main_thread(async move {
        let mux = Mux::get();
        if let Some(pane) = mux.get_pane(pane_id) {
            let dims = pane.get_dimensions();
            let viewport_range = dims.physical_top
                ..dims.physical_top + dims.viewport_rows as isize;

            let changed = pane.get_changed_since(viewport_range.clone(), prev_seqno);

            if changed.is_empty() && dims.physical_top == prev_physical_top {
                return None;
            }

            // Build map of changed row index → spans.
            // When physical_top changes, all viewport rows shifted — send them all.
            let mut lines_map = serde_json::Map::new();
            if dims.physical_top != prev_physical_top {
                // Viewport scrolled: every row now shows different content
                let (first_row, all_lines) = pane.get_lines(viewport_range);
                let spans = json_render::lines_to_spans(&all_lines);
                for (i, span_row) in spans.into_iter().enumerate() {
                    let row_idx = (first_row - dims.physical_top) as usize + i;
                    if row_idx >= dims.viewport_rows { break; }
                    lines_map.insert(
                        row_idx.to_string(),
                        serde_json::to_value(&span_row).unwrap_or(serde_json::Value::Null),
                    );
                }
            } else {
                for range in changed.iter() {
                    let (first_row, lines) = pane.get_lines(range.clone());
                    let spans = json_render::lines_to_spans(&lines);
                    for (i, span_row) in spans.into_iter().enumerate() {
                        let row_idx = (first_row - dims.physical_top) as usize + i;
                        if row_idx >= dims.viewport_rows { break; }
                        lines_map.insert(
                            row_idx.to_string(),
                            serde_json::to_value(&span_row).unwrap_or(serde_json::Value::Null),
                        );
                    }
                }
            }

            // Detect lines that scrolled off the viewport into scrollback.
            // If physical_top increased, those lines moved from viewport → scrollback.
            let scrollback_append = if dims.physical_top > prev_physical_top {
                let scroll_start = prev_physical_top;
                let scroll_end = dims.physical_top;
                let (_, scrolled_lines) = pane.get_lines(scroll_start..scroll_end);
                json_render::lines_to_spans(&scrolled_lines)
            } else {
                Vec::new()
            };

            // Get cursor position
            let cursor = pane.get_cursor_position();
            let cursor_y = ((cursor.y - dims.physical_top) as usize).min(dims.viewport_rows.saturating_sub(1));
            let cursor_info = json_render::CursorInfo {
                x: cursor.x,
                y: cursor_y,
                shape: json_render::cursor_shape_to_string(cursor.shape).to_string(),
                visible: cursor.visibility == termwiz::surface::CursorVisibility::Visible,
            };

            let new_seqno = get_max_seqno(&pane, &dims);
            Some((lines_map, scrollback_append, cursor_info, new_seqno, dims.physical_top))
        } else {
            None
        }
    })
    .await;

    if let Some((lines_map, scrollback_append, cursor_info, new_seqno, physical_top)) = data {
        *last_seqno = new_seqno;
        *last_physical_top = physical_top;
        let mut msg = serde_json::json!({
            "type": "screen_delta",
            "pane_id": pane_id,
            "lines": serde_json::Value::Object(lines_map),
            "cursor": cursor_info,
        });
        if !scrollback_append.is_empty() {
            msg["scrollback_append"] = serde_json::to_value(&scrollback_append)
                .unwrap_or(serde_json::Value::Null);
        }
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

/// Resize a pane's PTY to the given dimensions.
async fn resize_pane(pane_id: PaneId, cols: usize, rows: usize) -> anyhow::Result<()> {
    promise::spawn::spawn_into_main_thread(async move {
        let mux = Mux::get();
        if let Some(pane) = mux.get_pane(pane_id) {
            let size = TerminalSize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
                dpi: 0,
            };
            if let Err(e) = pane.resize(size) {
                log::error!("Failed to resize pane {}: {:#}", pane_id, e);
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


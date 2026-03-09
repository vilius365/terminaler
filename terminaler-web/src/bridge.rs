use crossbeam::channel;
use mux::MuxNotification;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Bridge between smol main thread (Mux notifications) and tokio (web server).
///
/// The Mux runs on a smol executor. Its `subscribe()` callback fires on that thread.
/// We use a crossbeam channel to send notifications to the tokio side, where a
/// background task reads them and broadcasts to all WebSocket sessions.
pub struct MuxBridge {
    /// Sender given to the Mux subscriber callback (smol side)
    pub smol_tx: channel::Sender<MuxNotification>,
    /// Receiver polled by the tokio bridge task
    pub smol_rx: channel::Receiver<MuxNotification>,
    /// Broadcast sender for all WebSocket sessions (tokio side)
    pub broadcast_tx: broadcast::Sender<MuxNotification>,
}

impl MuxBridge {
    pub fn new() -> Arc<Self> {
        let (smol_tx, smol_rx) = channel::unbounded();
        let (broadcast_tx, _) = broadcast::channel(256);
        Arc::new(Self {
            smol_tx,
            smol_rx,
            broadcast_tx,
        })
    }

    /// Subscribe to the Mux notification system.
    /// This must be called from the smol main thread.
    pub fn register_mux_subscriber(bridge: &Arc<Self>) {
        let tx = bridge.smol_tx.clone();
        let mux = mux::Mux::get();
        mux.subscribe(move |notification| {
            // Filter to only the notifications we care about
            match &notification {
                MuxNotification::PaneOutput(_)
                | MuxNotification::PaneAdded(_)
                | MuxNotification::PaneRemoved(_)
                | MuxNotification::TabResized(_) => {
                    // Ignore send errors (receiver dropped = web server stopped)
                    let _ = tx.send(notification);
                }
                _ => {}
            }
            true // keep subscription alive
        });
    }

    /// Spawn a tokio task that reads from the crossbeam channel and
    /// broadcasts to all WebSocket sessions.
    pub fn spawn_bridge_task(bridge: Arc<Self>) {
        let rx = bridge.smol_rx.clone();
        let tx = bridge.broadcast_tx.clone();

        tokio::task::spawn_blocking(move || {
            while let Ok(notification) = rx.recv() {
                // If no subscribers, the send will "fail" but that's fine
                let _ = tx.send(notification);
            }
        });
    }
}

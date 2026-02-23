pub mod ansi_render;
pub mod auth;
pub mod bridge;
pub mod server;
pub mod ws_session;

use bridge::MuxBridge;
use config::WebAccessConfig;
use server::AppState;
use std::sync::Arc;
use std::thread::JoinHandle;

/// Handle to the running web server, used for graceful shutdown.
pub struct WebServerHandle {
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    thread_handle: JoinHandle<()>,
}

impl WebServerHandle {
    /// Signal the web server to shut down and wait for the thread to finish.
    pub fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.thread_handle.join();
    }
}

/// Configuration for starting the web server.
pub struct WebConfig {
    pub bind_address: String,
    pub token: Option<String>,
}

impl From<&WebAccessConfig> for WebConfig {
    fn from(config: &WebAccessConfig) -> Self {
        Self {
            bind_address: config.bind_address.clone(),
            token: config.token.clone(),
        }
    }
}

/// Start the web access server on a dedicated OS thread with its own tokio runtime.
///
/// This function:
/// 1. Generates or loads the auth token
/// 2. Creates the smol↔tokio bridge
/// 3. Registers a Mux subscriber (must be called from smol main thread)
/// 4. Spawns a new OS thread with a tokio runtime
/// 5. Returns a handle for shutdown
pub fn start_web_server(config: WebConfig) -> anyhow::Result<WebServerHandle> {
    let token = auth::load_or_create_token(config.token.as_deref())?;
    let url = format!("http://{}/?token={}", config.bind_address, token);
    log::info!("Web access server starting on {}", url);
    write_status("STARTING");

    // Write URL to a well-known file for easy access
    if let Ok(url_path) = web_url_file_path() {
        if let Some(parent) = url_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&url_path, &url);
    }

    let bridge = MuxBridge::new();

    // Register the Mux subscriber on the current (smol main) thread
    MuxBridge::register_mux_subscriber(&bridge);

    let bind_address = config.bind_address.clone();
    let token_arc = Arc::new(token);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let thread_handle = std::thread::Builder::new()
        .name("web-server".into())
        .spawn({
            let bridge = bridge.clone();
            let token = token_arc.clone();
            move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("Failed to create tokio runtime for web server");

                rt.block_on(async {
                    // Start the bridge task that forwards smol notifications to tokio broadcast
                    MuxBridge::spawn_bridge_task(bridge.clone());

                    let state = AppState {
                        token,
                        bridge,
                    };
                    let app = server::build_router(state);

                    let listener = match tokio::net::TcpListener::bind(&bind_address).await {
                        Ok(l) => l,
                        Err(e) => {
                            log::error!("Failed to bind web server to {}: {:#}", bind_address, e);
                            write_status(&format!("BIND FAILED: {:#}", e));
                            return;
                        }
                    };

                    log::info!("Web access server listening on {}", bind_address);
                    write_status(&format!("LISTENING on {}", bind_address));

                    axum::serve(listener, app)
                        .with_graceful_shutdown(async {
                            let _ = shutdown_rx.await;
                            write_status("SHUTDOWN signal received");
                        })
                        .await
                        .unwrap_or_else(|e| {
                            log::error!("Web server error: {:#}", e);
                        });
                });
            }
        })?;

    Ok(WebServerHandle {
        shutdown_tx,
        thread_handle,
    })
}

fn write_status(msg: &str) {
    if let Ok(url_path) = web_url_file_path() {
        let status_path = url_path.with_file_name("web-status");
        let _ = std::fs::write(&status_path, msg);
    }
}

fn web_url_file_path() -> anyhow::Result<std::path::PathBuf> {
    if cfg!(windows) {
        let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        Ok(std::path::PathBuf::from(appdata)
            .join("Terminaler")
            .join("web-url"))
    } else {
        Ok(dirs_next::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".config"))
            .join("terminaler")
            .join("web-url"))
    }
}

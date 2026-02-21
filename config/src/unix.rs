// Minimal UnixDomain and UnixTarget stubs for Phase 0.
// The full unix domain mux support will be revisited in Phase 5 (Session persistence).
// Note: default_true is defined in config/src/lib.rs but not pub-exported; define locally.
#[allow(dead_code)]
fn default_true_local() -> bool {
    true
}
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// The target connection endpoint for a Unix domain socket connection.
#[derive(Clone, Debug)]
pub enum UnixTarget {
    /// A filesystem socket path.
    Socket(PathBuf),
    /// A proxy command to pipe through.
    Proxy(Vec<String>),
}

/// Minimal configuration for a Unix domain socket mux domain.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UnixDomain {
    /// Name of this domain (used as an identifier).
    #[serde(default)]
    pub name: String,

    /// Path to the unix domain socket.
    #[serde(default)]
    pub socket_path: Option<PathBuf>,

    /// If true, do not attempt to start the server automatically.
    #[serde(default)]
    pub no_serve_automatically: bool,

    /// If true, skip unix socket permissions check.
    #[serde(default)]
    pub skip_permissions_check: bool,

    /// Connect automatically on startup.
    #[serde(default)]
    pub connect_automatically: bool,

    /// Milliseconds before showing local echo lag indicator.
    #[serde(default)]
    pub local_echo_threshold_ms: Option<u64>,

    /// Show lag indicator overlay.
    #[serde(default = "default_true_local")]
    pub overlay_lag_indicator: bool,

    /// Read timeout for the socket connection.
    #[serde(default = "default_read_timeout")]
    pub read_timeout: Duration,

    /// Write timeout for the socket connection.
    #[serde(default = "default_write_timeout")]
    pub write_timeout: Duration,
}

fn default_read_timeout() -> Duration {
    Duration::from_secs(60)
}

fn default_write_timeout() -> Duration {
    Duration::from_secs(60)
}

impl UnixDomain {
    /// Returns the socket path, defaulting to a platform-specific location.
    pub fn socket_path(&self) -> PathBuf {
        self.socket_path.clone().unwrap_or_else(|| {
            crate::RUNTIME_DIR.join("sock")
        })
    }

    /// Returns the connection target for this domain.
    pub fn target(&self) -> UnixTarget {
        UnixTarget::Socket(self.socket_path())
    }

    /// Build the argv for spawning the mux server.
    pub fn serve_command(&self) -> anyhow::Result<Vec<String>> {
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "terminaler".to_string());
        Ok(vec![exe, "start".to_string(), "--always-new-process".to_string()])
    }
}

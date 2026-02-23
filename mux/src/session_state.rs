//! Session state serialization for save/restore.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Serializable session state for the entire mux.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    /// Windows in the session.
    pub windows: Vec<WindowState>,
    /// Active workspace name.
    #[serde(default)]
    pub active_workspace: Option<String>,
}

/// State of a single window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    /// Window title.
    pub title: String,
    /// Tabs in this window.
    pub tabs: Vec<TabState>,
    /// Index of the active tab.
    pub active_tab_index: usize,
    /// Window position (x, y) if known.
    #[serde(default)]
    pub position: Option<(i32, i32)>,
    /// Window size (width, height) in pixels.
    #[serde(default)]
    pub size: Option<(u32, u32)>,
}

/// State of a single tab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabState {
    /// Tab title.
    pub title: String,
    /// Pane layout tree.
    pub layout: PaneLayoutNode,
}

/// Serializable pane layout tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PaneLayoutNode {
    /// A terminal pane leaf.
    Pane {
        /// Working directory of the pane.
        #[serde(default)]
        cwd: Option<PathBuf>,
        /// The command that was running (for display/restart).
        #[serde(default)]
        command: Option<String>,
        /// Whether this pane was the active pane.
        #[serde(default)]
        is_active: bool,
    },
    /// A split containing two children.
    Split {
        /// Direction of the split.
        direction: SplitDirectionState,
        /// Size ratio of the first child (0.0-1.0).
        ratio: f64,
        /// First (left/top) child.
        first: Box<PaneLayoutNode>,
        /// Second (right/bottom) child.
        second: Box<PaneLayoutNode>,
    },
}

/// Split direction for serialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirectionState {
    Horizontal,
    Vertical,
}

/// Default session file path.
pub fn session_file_path() -> PathBuf {
    if let Some(ref dir) = *config::PORTABLE_DIR {
        return dir.join("sessions").join("last-session.json");
    }
    if cfg!(windows) {
        let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(appdata)
            .join("Terminaler")
            .join("sessions")
            .join("last-session.json")
    } else {
        dirs_next::config_dir()
            .unwrap_or_else(|| PathBuf::from(".config"))
            .join("terminaler")
            .join("sessions")
            .join("last-session.json")
    }
}

/// Save session state to a JSON file.
pub fn save_session(state: &SessionState) -> anyhow::Result<()> {
    let path = session_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(&path, json)?;
    log::info!("Session saved to {}", path.display());
    Ok(())
}

/// Delete the session file (called after successful restore).
pub fn delete_session() -> anyhow::Result<()> {
    let path = session_file_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// Load session state from a JSON file.
pub fn load_session() -> anyhow::Result<Option<SessionState>> {
    let path = session_file_path();
    if !path.exists() {
        return Ok(None);
    }
    let json = std::fs::read_to_string(&path)?;
    let state: SessionState = serde_json::from_str(&json)?;
    log::info!("Session loaded from {}", path.display());
    Ok(Some(state))
}

impl SessionState {
    /// Create an empty session state.
    pub fn empty() -> Self {
        Self {
            windows: Vec::new(),
            active_workspace: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_empty_session() {
        let state = SessionState::empty();
        let json = serde_json::to_string(&state).unwrap();
        let parsed: SessionState = serde_json::from_str(&json).unwrap();
        assert!(parsed.windows.is_empty());
    }

    #[test]
    fn test_serialize_session_with_tabs() {
        let state = SessionState {
            windows: vec![WindowState {
                title: "Main".into(),
                tabs: vec![
                    TabState {
                        title: "Shell".into(),
                        layout: PaneLayoutNode::Pane {
                            cwd: Some(PathBuf::from("/home/user")),
                            command: Some("bash".into()),
                            is_active: true,
                        },
                    },
                    TabState {
                        title: "Dev".into(),
                        layout: PaneLayoutNode::Split {
                            direction: SplitDirectionState::Horizontal,
                            ratio: 0.5,
                            first: Box::new(PaneLayoutNode::Pane {
                                cwd: Some(PathBuf::from("/home/user/project")),
                                command: None,
                                is_active: true,
                            }),
                            second: Box::new(PaneLayoutNode::Pane {
                                cwd: Some(PathBuf::from("/home/user/project")),
                                command: None,
                                is_active: false,
                            }),
                        },
                    },
                ],
                active_tab_index: 0,
                position: Some((100, 200)),
                size: Some((1920, 1080)),
            }],
            active_workspace: Some("default".into()),
        };
        let json = serde_json::to_string_pretty(&state).unwrap();
        let parsed: SessionState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.windows.len(), 1);
        assert_eq!(parsed.windows[0].tabs.len(), 2);
    }

    #[test]
    fn test_session_file_path() {
        let path = session_file_path();
        assert!(
            path.to_string_lossy().contains("terminaler")
                || path.to_string_lossy().contains("Terminaler")
        );
    }
}

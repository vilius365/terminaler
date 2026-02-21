//! Session save/restore integration for the GUI.

use mux::pane::CachePolicy;
use mux::session_state::{
    PaneLayoutNode, SessionState, SplitDirectionState, TabState, WindowState,
};
use mux::Mux;
use std::path::PathBuf;

/// Capture the current mux state as a serializable session.
pub fn capture_session() -> SessionState {
    let mux = Mux::get();
    let mut windows = Vec::new();

    for window_id in mux.iter_windows() {
        let mux_window = match mux.get_window(window_id) {
            Some(w) => w,
            None => continue,
        };

        let active_tab_idx = mux_window.get_active_idx();
        let mut tabs = Vec::new();

        for tab in mux_window.iter() {
            let title = tab
                .get_active_pane()
                .map(|p| p.get_title())
                .unwrap_or_default();

            let panes = tab.iter_panes();
            // Build a simple layout from positioned panes
            let layout = if panes.len() <= 1 {
                PaneLayoutNode::Pane {
                    cwd: panes
                        .first()
                        .and_then(|p| p.pane.get_current_working_dir(CachePolicy::FetchImmediate))
                        .and_then(|u| u.to_file_path().ok())
                        .map(|p| p.into()),
                    command: None,
                    is_active: true,
                }
            } else {
                // For multi-pane layouts, capture as a flat list
                // (full tree reconstruction would require walking the bintree)
                build_layout_from_panes(&panes)
            };

            tabs.push(TabState { title, layout });
        }

        windows.push(WindowState {
            title: mux_window.get_title().to_string(),
            tabs,
            active_tab_index: active_tab_idx,
            position: None,
            size: None,
        });
    }

    SessionState {
        windows,
        active_workspace: Some(mux.active_workspace().to_string()),
    }
}

fn build_layout_from_panes(panes: &[mux::tab::PositionedPane]) -> PaneLayoutNode {
    if panes.is_empty() {
        return PaneLayoutNode::Pane {
            cwd: None,
            command: None,
            is_active: false,
        };
    }
    if panes.len() == 1 {
        let p = &panes[0];
        return PaneLayoutNode::Pane {
            cwd: p.pane.get_current_working_dir(CachePolicy::FetchImmediate)
                .and_then(|u| u.to_file_path().ok())
                .map(|p| p.into()),
            command: None,
            is_active: p.is_active,
        };
    }

    // Simple heuristic: split the pane list in half, creating a vertical split
    let mid = panes.len() / 2;
    let (left, right) = panes.split_at(mid);
    PaneLayoutNode::Split {
        direction: SplitDirectionState::Horizontal,
        ratio: 0.5,
        first: Box::new(build_layout_from_panes(left)),
        second: Box::new(build_layout_from_panes(right)),
    }
}

/// Save the current session state to disk.
pub fn save_current_session() {
    let state = capture_session();
    if let Err(e) = mux::session_state::save_session(&state) {
        log::error!("Failed to save session: {:#}", e);
    }
}

/// Try to load the previous session state from disk.
pub fn load_previous_session() -> Option<SessionState> {
    match mux::session_state::load_session() {
        Ok(state) => state,
        Err(e) => {
            log::warn!("Failed to load session: {:#}", e);
            None
        }
    }
}

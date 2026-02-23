//! Session save/restore integration for the GUI.

use mux::domain::{Domain, SplitSource};
use mux::session_state::{
    PaneLayoutNode, SessionState, SplitDirectionState, TabState, WindowState,
};
use mux::tab::{SplitDirection, SplitRequest, SplitSize, TabId};
use mux::Mux;
use std::sync::Arc;
use terminaler_term::TerminalSize;

/// Capture the current mux state as a serializable session.
/// Uses Tab::session_layout_tree() which walks the bintree to capture
/// actual split directions and size ratios.
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
            let layout = tab.session_layout_tree();
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

/// Restore a previously saved session, recreating windows, tabs, and pane splits.
pub async fn restore_session(
    session: &SessionState,
    domain: &Arc<dyn Domain>,
    size: TerminalSize,
) -> anyhow::Result<()> {
    let mux = Mux::get();

    for win_state in &session.windows {
        let window_id = *mux.new_empty_window(session.active_workspace.clone(), None);

        for (tab_idx, tab_state) in win_state.tabs.iter().enumerate() {
            // Spawn the first pane from the leftmost/topmost leaf
            let first_cwd = first_leaf_cwd(&tab_state.layout);

            if tab_idx == 0 {
                // First tab: spawn into the window that was just created
                let tab = domain.spawn(size, None, first_cwd, window_id).await?;
                let root_pane_id = tab
                    .get_active_pane()
                    .map(|p| p.pane_id())
                    .ok_or_else(|| anyhow::anyhow!("No active pane after spawn"))?;

                // Recursively split to rebuild the tree
                restore_subtree(
                    &tab_state.layout,
                    domain,
                    tab.tab_id(),
                    root_pane_id,
                )
                .await?;

                // Set active pane
                if let Some(active_idx) = active_pane_index(&tab_state.layout) {
                    let panes = tab.iter_panes();
                    if active_idx < panes.len() {
                        tab.set_active_idx(active_idx);
                    }
                }
            } else {
                // Additional tabs: spawn a new tab in the same window
                let tab = domain.spawn(size, None, first_cwd, window_id).await?;
                let root_pane_id = tab
                    .get_active_pane()
                    .map(|p| p.pane_id())
                    .ok_or_else(|| anyhow::anyhow!("No active pane after spawn"))?;

                restore_subtree(
                    &tab_state.layout,
                    domain,
                    tab.tab_id(),
                    root_pane_id,
                )
                .await?;

                if let Some(active_idx) = active_pane_index(&tab_state.layout) {
                    let panes = tab.iter_panes();
                    if active_idx < panes.len() {
                        tab.set_active_idx(active_idx);
                    }
                }
            }
        }

        // Set active tab
        if let Some(mut mux_win) = mux.get_window_mut(window_id) {
            mux_win.set_active_without_saving(win_state.active_tab_index);
        }
    }

    // Delete session file after successful restore
    if let Err(e) = mux::session_state::delete_session() {
        log::warn!("Failed to delete session file: {:#}", e);
    }

    Ok(())
}

/// Recursively walk the PaneLayoutNode tree, splitting panes to rebuild the layout.
/// The first leaf (leftmost/topmost) is assumed to already exist as `pane_id`.
async fn restore_subtree(
    node: &PaneLayoutNode,
    domain: &Arc<dyn Domain>,
    tab_id: TabId,
    pane_id: mux::pane::PaneId,
) -> anyhow::Result<()> {
    match node {
        PaneLayoutNode::Pane { .. } => {
            // Leaf node — the pane already exists (spawned by domain.spawn or a prior split).
            Ok(())
        }
        PaneLayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let split_dir = match direction {
                SplitDirectionState::Horizontal => SplitDirection::Horizontal,
                SplitDirectionState::Vertical => SplitDirection::Vertical,
            };
            let size_pct = ((*ratio) * 100.0).round() as u8;

            // Get cwd for the second child's first leaf
            let second_cwd = first_leaf_cwd(second);

            // Split the current pane — the new pane goes into the "second" position
            let new_pane = domain
                .split_pane(
                    SplitSource::Spawn {
                        command: None,
                        command_dir: second_cwd,
                    },
                    tab_id,
                    pane_id,
                    SplitRequest {
                        direction: split_dir,
                        target_is_second: true,
                        top_level: false,
                        size: SplitSize::Percent(size_pct),
                    },
                )
                .await?;

            let new_pane_id = new_pane.pane_id();

            // Recurse into first child (using the original pane_id)
            Box::pin(restore_subtree(first, domain, tab_id, pane_id)).await?;
            // Recurse into second child (using the newly created pane)
            Box::pin(restore_subtree(second, domain, tab_id, new_pane_id)).await?;

            Ok(())
        }
    }
}

/// Extract the cwd from the first (leftmost/topmost) leaf in a layout tree.
fn first_leaf_cwd(node: &PaneLayoutNode) -> Option<String> {
    match node {
        PaneLayoutNode::Pane { cwd, .. } => cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
        PaneLayoutNode::Split { first, .. } => first_leaf_cwd(first),
    }
}

/// Find the index of the active pane in the layout tree (in-order traversal).
fn active_pane_index(node: &PaneLayoutNode) -> Option<usize> {
    let mut idx = 0;
    find_active_index(node, &mut idx)
}

fn find_active_index(node: &PaneLayoutNode, idx: &mut usize) -> Option<usize> {
    match node {
        PaneLayoutNode::Pane { is_active, .. } => {
            if *is_active {
                Some(*idx)
            } else {
                *idx += 1;
                None
            }
        }
        PaneLayoutNode::Split { first, second, .. } => {
            find_active_index(first, idx).or_else(|| find_active_index(second, idx))
        }
    }
}

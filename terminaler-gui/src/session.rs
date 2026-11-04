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
            let title = tab.get_title();
            let layout = tab.session_layout_tree();
            tabs.push(TabState { title, layout });
        }

        windows.push(WindowState {
            title: mux_window.get_title().to_string(),
            workspace: Some(mux_window.get_workspace().to_string()),
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

/// The last session text written, so an unchanged layout is not rewritten.
static LAST_SAVED: parking_lot::Mutex<String> = parking_lot::Mutex::new(String::new());

/// Save the current session state to disk. An empty mux is never saved: by
/// the time the last window has closed there is nothing to capture, and
/// writing that would erase the layout worth restoring.
pub fn save_current_session() {
    let state = capture_session();
    if state.windows.is_empty() {
        return;
    }
    let text = match serde_json::to_string(&state) {
        Ok(text) => text,
        Err(e) => {
            log::error!("Failed to encode session: {:#}", e);
            return;
        }
    };
    {
        let mut last = LAST_SAVED.lock();
        if *last == text {
            return;
        }
        *last = text;
    }
    if let Err(e) = mux::session_state::save_session(&state) {
        log::error!("Failed to save session: {:#}", e);
    }
}

/// Save the session every few seconds while the GUI runs, so a kill, a
/// crash or a restart from outside still leaves the latest layout behind.
pub fn start_autosave() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(start_autosave_thread);
}

fn start_autosave_thread() {
    std::thread::Builder::new()
        .name("session-autosave".into())
        .spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            promise::spawn::spawn_into_main_thread(async {
                save_current_session();
            })
            .detach();
        })
        .ok();
}

fn command_for(argv: Option<Vec<String>>) -> Option<portable_pty::CommandBuilder> {
    argv.filter(|a| !a.is_empty()).map(|a| {
        portable_pty::CommandBuilder::from_argv(a.into_iter().map(std::ffi::OsString::from).collect())
    })
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
        let workspace = win_state
            .workspace
            .clone()
            .or_else(|| session.active_workspace.clone());
        let window_id = *mux.new_empty_window(workspace, None);

        for (tab_idx, tab_state) in win_state.tabs.iter().enumerate() {
            // Spawn the first pane from the leftmost/topmost leaf
            let first_cwd = first_leaf_cwd(&tab_state.layout);
            let mut hidden_panes = Vec::new();

            if tab_idx == 0 {
                // First tab: spawn into the window that was just created
                let tab = domain
                    .spawn(size, command_for(first_leaf_argv(&tab_state.layout)), first_cwd, window_id)
                    .await?;
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
                    &mut hidden_panes,
                )
                .await?;

                if !tab_state.title.is_empty() {
                    tab.set_title(&tab_state.title);
                }

                // Set active pane
                if let Some(active_idx) = active_pane_index(&tab_state.layout) {
                    let panes = tab.iter_panes();
                    if active_idx < panes.len() {
                        tab.set_active_idx(active_idx);
                    }
                }

                for pane_id in hidden_panes {
                    if !tab.is_pane_hidden(pane_id) {
                        tab.toggle_pane_hidden(pane_id);
                    }
                }

                tab.restore_groups(&tab_state.layout);
            } else {
                // Additional tabs: spawn a new tab in the same window
                let tab = domain
                    .spawn(size, command_for(first_leaf_argv(&tab_state.layout)), first_cwd, window_id)
                    .await?;
                let root_pane_id = tab
                    .get_active_pane()
                    .map(|p| p.pane_id())
                    .ok_or_else(|| anyhow::anyhow!("No active pane after spawn"))?;

                restore_subtree(
                    &tab_state.layout,
                    domain,
                    tab.tab_id(),
                    root_pane_id,
                    &mut hidden_panes,
                )
                .await?;

                if !tab_state.title.is_empty() {
                    tab.set_title(&tab_state.title);
                }

                if let Some(active_idx) = active_pane_index(&tab_state.layout) {
                    let panes = tab.iter_panes();
                    if active_idx < panes.len() {
                        tab.set_active_idx(active_idx);
                    }
                }

                for pane_id in hidden_panes {
                    if !tab.is_pane_hidden(pane_id) {
                        tab.toggle_pane_hidden(pane_id);
                    }
                }

                tab.restore_groups(&tab_state.layout);
            }
        }

        // Set active tab
        if let Some(mut mux_win) = mux.get_window_mut(window_id) {
            if !win_state.title.is_empty() {
                mux_win.set_title(&win_state.title);
            }
            mux_win.set_active_without_saving(win_state.active_tab_index);
        }
    }

    if let Some(active_workspace) = &session.active_workspace {
        mux.set_active_workspace(active_workspace);
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
    hidden_panes: &mut Vec<mux::pane::PaneId>,
) -> anyhow::Result<()> {
    match node {
        PaneLayoutNode::Pane { hidden, .. } => {
            // Leaf node — the pane already exists (spawned by domain.spawn or a prior split).
            if *hidden {
                hidden_panes.push(pane_id);
            }
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
            let size_pct = second_percent(*ratio);

            // Get cwd for the second child's first leaf
            let second_cwd = first_leaf_cwd(second);

            // Split the current pane — the new pane goes into the "second" position
            let new_pane = domain
                .split_pane(
                    SplitSource::Spawn {
                        command: command_for(first_leaf_argv(second)),
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
            Box::pin(restore_subtree(first, domain, tab_id, pane_id, hidden_panes)).await?;
            // Recurse into second child (using the newly created pane)
            Box::pin(restore_subtree(second, domain, tab_id, new_pane_id, hidden_panes)).await?;

            Ok(())
        }
    }
}

/// Percent of a split the new (second) pane gets, given the saved size ratio
/// of the first child. `SplitRequest` sizes the NEW pane when it goes second,
/// so passing the first child's share would mirror every lopsided layout.
fn second_percent(first_ratio: f64) -> u8 {
    (((1.0 - first_ratio) * 100.0).round() as i64).clamp(1, 99) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_pane_gets_the_rest_of_the_split() {
        assert_eq!(second_percent(0.5), 50);
        assert_eq!(second_percent(0.693), 31);
        assert_eq!(second_percent(0.0), 99);
        assert_eq!(second_percent(1.0), 1);
    }
}

/// The ssh argv of the first (leftmost/topmost) leaf, if it was running one.
fn first_leaf_argv(node: &PaneLayoutNode) -> Option<Vec<String>> {
    match node {
        PaneLayoutNode::Pane { argv, .. } => argv.clone(),
        PaneLayoutNode::Split { first, .. } => first_leaf_argv(first),
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

use anyhow::{anyhow, bail, Context};
use config::keyassignment::SpawnCommand;
use config::TermConfig;
use mux::activity::Activity;
use mux::domain::SplitSource;
use mux::tab::SplitRequest;
use mux::window::WindowId as MuxWindowId;
use mux::Mux;
use portable_pty::CommandBuilder;
use std::sync::Arc;
use terminaler_term::TerminalSize;

#[derive(Copy, Debug, Clone, Eq, PartialEq)]
pub enum SpawnWhere {
    NewWindow,
    NewTab,
    SplitPane(SplitRequest),
}

pub fn spawn_command_impl(
    spawn: &SpawnCommand,
    spawn_where: SpawnWhere,
    size: TerminalSize,
    src_window_id: Option<MuxWindowId>,
    term_config: Arc<TermConfig>,
) {
    let spawn = spawn.clone();

    promise::spawn::spawn(async move {
        if let Err(err) =
            spawn_command_internal(spawn, spawn_where, size, src_window_id, term_config).await
        {
            log::error!("Failed to spawn: {:#}", err);
        }
    })
    .detach();
}

/// Panes this GUI spawned to attach a tmux session, keyed by pane id. Panes
/// attached by hand outside Terminaler are not recorded.
static TMUX_ATTACH_PANES: parking_lot::Mutex<Vec<(mux::pane::PaneId, (String, String))>> =
    parking_lot::Mutex::new(Vec::new());

/// Label given to a tmux attach spawn; `parse_tmux_attach_label` reverses it.
pub fn tmux_attach_label(box_name: &str, session: &str) -> String {
    format!("tmux {}:{}", box_name, session)
}

/// tmux session names cannot contain `:`, so the last `:` splits the label.
pub fn parse_tmux_attach_label(label: &str) -> Option<(String, String)> {
    let rest = label.strip_prefix("tmux ")?;
    let (box_name, session) = rest.rsplit_once(':')?;
    Some((box_name.to_string(), session.to_string()))
}

/// Live pane previously spawned to attach `box_name:session`, dropping
/// records for panes that no longer exist.
pub fn live_tmux_attach_pane(box_name: &str, session: &str) -> Option<mux::pane::PaneId> {
    let mux = Mux::try_get()?;
    let mut panes = TMUX_ATTACH_PANES.lock();
    panes.retain(|(id, _)| mux.get_pane(*id).is_some());
    panes
        .iter()
        .rev()
        .find(|(_, (b, s))| b == box_name && s == session)
        .map(|(id, _)| *id)
}

fn record_tmux_attach_pane(pane_id: mux::pane::PaneId, key: (String, String)) {
    TMUX_ATTACH_PANES.lock().push((pane_id, key));
}

pub async fn spawn_command_internal(
    spawn: SpawnCommand,
    spawn_where: SpawnWhere,
    size: TerminalSize,
    src_window_id: Option<MuxWindowId>,
    term_config: Arc<TermConfig>,
) -> anyhow::Result<()> {
    let mux = Mux::get();
    let activity = Activity::new();
    let attach_key = spawn.label.as_deref().and_then(parse_tmux_attach_label);

    let current_pane_id = match src_window_id {
        Some(window_id) => {
            if let Some(tab) = mux.get_active_tab_for_window(window_id) {
                tab.get_active_pane().map(|p| p.pane_id())
            } else {
                None
            }
        }
        None => None,
    };

    let cwd = if let Some(cwd) = spawn.cwd.as_ref() {
        Some(cwd.to_str().map(|s| s.to_owned()).ok_or_else(|| {
            anyhow!(
                "Domain::spawn requires that the cwd be unicode in {:?}",
                cwd
            )
        })?)
    } else {
        None
    };

    let cmd_builder = match (
        spawn.args.as_ref(),
        spawn.cwd.as_ref(),
        spawn.set_environment_variables.is_empty(),
    ) {
        (None, None, true) => None,
        _ => {
            let mut builder = spawn
                .args
                .as_ref()
                .map(|args| CommandBuilder::from_argv(args.iter().map(Into::into).collect()))
                .unwrap_or_else(CommandBuilder::new_default_prog);
            for (k, v) in spawn.set_environment_variables.iter() {
                builder.env(k, v);
            }
            if let Some(cwd) = &spawn.cwd {
                builder.cwd(cwd);
            }
            Some(builder)
        }
    };

    let workspace = mux.active_workspace().clone();

    match spawn_where {
        SpawnWhere::SplitPane(direction) => {
            let src_window_id = match src_window_id {
                Some(id) => id,
                None => anyhow::bail!("no src window when splitting a pane?"),
            };
            if let Some(tab) = mux.get_active_tab_for_window(src_window_id) {
                let pane = tab
                    .get_active_pane()
                    .ok_or_else(|| anyhow!("tab to have a pane"))?;

                log::trace!("doing split_pane");
                let (pane, _size) = mux
                    .split_pane(
                        // tab.tab_id(),
                        pane.pane_id(),
                        direction,
                        SplitSource::Spawn {
                            command: cmd_builder,
                            command_dir: cwd,
                        },
                        spawn.domain,
                    )
                    .await
                    .context("split_pane")?;
                if let Some(key) = attach_key.clone() {
                    record_tmux_attach_pane(pane.pane_id(), key);
                }
                pane.set_config(term_config);
            } else {
                bail!("there is no active tab while splitting pane!?");
            }
        }
        _ => {
            let (_tab, pane, window_id) = mux
                .spawn_tab_or_window(
                    match spawn_where {
                        SpawnWhere::NewWindow => None,
                        _ => src_window_id,
                    },
                    spawn.domain,
                    cmd_builder,
                    cwd,
                    size,
                    current_pane_id,
                    workspace,
                    spawn.position,
                )
                .await
                .context("spawn_tab_or_window")?;

            if let Some(key) = attach_key {
                record_tmux_attach_pane(pane.pane_id(), key);
            }

            // If it was created in this window, it copies our handlers.
            // Otherwise, we'll pick them up when we later respond to
            // the new window being created.
            if Some(window_id) == src_window_id {
                pane.set_config(term_config);
            }
        }
    };

    drop(activity);

    Ok(())
}

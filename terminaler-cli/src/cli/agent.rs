//! `terminaler cli agent`: read and wait on the state of the Claude Code
//! session in a pane. The state is the `claude_status` user variable that the
//! Claude Code lifecycle hook sets with OSC 1337 `SetUserVar`, so it follows
//! Claude's own events, not what the screen shows.

use crate::cli::CliOutputFormatKind;
use clap::{Parser, Subcommand, ValueEnum};
use mux::pane::PaneId;
use serde::Serialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use terminaler_client::client::Client;

#[derive(Debug, Subcommand, Clone)]
pub enum AgentSubCommand {
    /// List the panes that run a Claude Code session, with their state.
    #[command(name = "list", rename_all = "kebab")]
    List(AgentList),

    /// Wait until a pane's Claude Code session reaches a state.
    ///
    /// Exits 0 when the state is reached (at once if it already is), and
    /// non-zero on timeout or when the pane goes away.
    #[command(name = "wait", rename_all = "kebab")]
    Wait(AgentWait),
}

#[derive(Debug, Parser, Clone)]
pub struct AgentCommand {
    #[command(subcommand)]
    sub: AgentSubCommand,
}

impl AgentCommand {
    pub async fn run(&self, client: Client) -> anyhow::Result<()> {
        match &self.sub {
            AgentSubCommand::List(cmd) => cmd.run(client).await,
            AgentSubCommand::Wait(cmd) => cmd.run(client).await,
        }
    }
}

/// What a pane's Claude Code session is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// A turn is running.
    Working,
    /// Claude asked for permission or input and waits for an answer.
    Blocked,
    /// The turn ended.
    Idle,
    /// The last event was an error.
    Error,
}

impl AgentState {
    fn name(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Idle => "idle",
            Self::Error => "error",
        }
    }
}

/// The state in a pane's user variables, or `None` when no Claude Code
/// session has set one (the hook clears the variable on session end).
pub fn agent_state(user_vars: &HashMap<String, String>) -> Option<AgentState> {
    let raw = user_vars.get("claude_status")?;
    match raw.as_str() {
        "" => None,
        "waiting_input" => Some(AgentState::Blocked),
        "idle" => Some(AgentState::Idle),
        "error" => Some(AgentState::Error),
        // The hook writes `working` for every other lifecycle event, and the
        // GUI maps unknown values to working too.
        _ => Some(AgentState::Working),
    }
}

#[derive(Debug, Serialize)]
struct AgentRow {
    pane_id: PaneId,
    state: AgentState,
    model: Option<String>,
    cwd: Option<String>,
}

#[derive(Debug, Parser, Clone, Copy)]
pub struct AgentList {
    /// Controls the output format: "table" or "json".
    #[arg(long = "format", default_value = "table")]
    format: CliOutputFormatKind,
}

impl AgentList {
    pub async fn run(&self, client: Client) -> anyhow::Result<()> {
        let panes = client.list_panes().await?;
        let mut rows = vec![];
        for tabroot in panes.tabs {
            let mut cursor = tabroot.into_tree().cursor();
            loop {
                if let Some(entry) = cursor.leaf_mut() {
                    let vars = client
                        .get_pane_user_vars(codec::GetPaneUserVars {
                            pane_id: entry.pane_id,
                        })
                        .await?
                        .user_vars;
                    if let Some(state) = agent_state(&vars) {
                        rows.push(AgentRow {
                            pane_id: entry.pane_id,
                            state,
                            model: vars.get("claude_model").filter(|v| !v.is_empty()).cloned(),
                            cwd: entry.working_dir.as_ref().map(|u| u.url.to_string()),
                        });
                    }
                }
                match cursor.preorder_next() {
                    Ok(c) => cursor = c,
                    Err(_) => break,
                }
            }
        }
        match self.format {
            CliOutputFormatKind::Json => {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            }
            CliOutputFormatKind::Table => {
                let cols = vec![
                    tabout::Column {
                        name: "PANEID".to_string(),
                        alignment: tabout::Alignment::Right,
                    },
                    tabout::Column {
                        name: "STATE".to_string(),
                        alignment: tabout::Alignment::Left,
                    },
                    tabout::Column {
                        name: "MODEL".to_string(),
                        alignment: tabout::Alignment::Left,
                    },
                    tabout::Column {
                        name: "CWD".to_string(),
                        alignment: tabout::Alignment::Left,
                    },
                ];
                let data = rows
                    .iter()
                    .map(|r| {
                        vec![
                            r.pane_id.to_string(),
                            r.state.name().to_string(),
                            r.model.clone().unwrap_or_default(),
                            r.cwd.clone().unwrap_or_default(),
                        ]
                    })
                    .collect::<Vec<_>>();
                tabout::tabulate_output(&cols, &data, &mut std::io::stdout().lock())?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Parser, Clone)]
pub struct AgentWait {
    /// The pane to watch. The default is the current pane, from the
    /// environment variable TERMINALER_PANE.
    #[arg(long)]
    pane_id: Option<PaneId>,

    /// The state to wait for.
    #[arg(long, value_enum)]
    until: AgentState,

    /// Give up after this many seconds. The default waits forever.
    #[arg(long)]
    timeout: Option<u64>,
}

impl AgentWait {
    pub async fn run(&self, client: Client) -> anyhow::Result<()> {
        let pane_id = client.resolve_pane_id(self.pane_id).await?;
        let deadline = self
            .timeout
            .map(|secs| Instant::now() + Duration::from_secs(secs));
        loop {
            let vars = client
                .get_pane_user_vars(codec::GetPaneUserVars { pane_id })
                .await?
                .user_vars;
            if agent_state(&vars) == Some(self.until) {
                return Ok(());
            }
            if let Some(deadline) = deadline {
                if Instant::now() >= deadline {
                    anyhow::bail!(
                        "timed out waiting for pane {} to be {}",
                        pane_id,
                        self.until.name()
                    );
                }
            }
            smol::Timer::after(Duration::from_millis(500)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(status: Option<&str>) -> HashMap<String, String> {
        let mut m = HashMap::new();
        if let Some(s) = status {
            m.insert("claude_status".to_string(), s.to_string());
        }
        m
    }

    #[test]
    fn maps_hook_values_to_states() {
        assert_eq!(agent_state(&vars(Some("working"))), Some(AgentState::Working));
        assert_eq!(agent_state(&vars(Some("waiting_input"))), Some(AgentState::Blocked));
        assert_eq!(agent_state(&vars(Some("idle"))), Some(AgentState::Idle));
        assert_eq!(agent_state(&vars(Some("error"))), Some(AgentState::Error));
        assert_eq!(agent_state(&vars(Some("something-new"))), Some(AgentState::Working));
    }

    #[test]
    fn no_session_means_no_state() {
        assert_eq!(agent_state(&vars(None)), None);
        assert_eq!(agent_state(&vars(Some(""))), None);
    }
}

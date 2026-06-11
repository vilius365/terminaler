use std::path::PathBuf;
use terminaler_dynamic::{FromDynamic, ToDynamic};

/// Configuration for the Claude agent orchestration features
/// (NewClaudeAgent / AgentDashboard actions).
#[derive(Debug, Clone, FromDynamic, ToDynamic)]
pub struct ClaudeAgentConfig {
    /// Directory under which agent worktrees are created.
    /// Defaults to `<repo-parent>/<repo-name>-worktrees/`.
    #[dynamic(default)]
    pub worktree_root: Option<PathBuf>,

    /// Command used to launch the agent in the main pane.
    /// Defaults to `["claude"]`.
    #[dynamic(default = "default_claude_command")]
    pub claude_command: Vec<String>,

    /// Workspace template used for the spawned agent tab.
    /// Defaults to "claude-code".
    #[dynamic(default = "default_template")]
    pub template: String,
}

impl Default for ClaudeAgentConfig {
    fn default() -> Self {
        Self {
            worktree_root: None,
            claude_command: default_claude_command(),
            template: default_template(),
        }
    }
}

fn default_claude_command() -> Vec<String> {
    vec!["claude".to_string()]
}

fn default_template() -> String {
    "claude-code".to_string()
}

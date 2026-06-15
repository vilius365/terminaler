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
    // On Windows, `claude` is typically a `claude.cmd`/`.ps1`/extensionless
    // npm shim — there is no `claude.exe`. portable-pty resolves the exact
    // name before PATHEXT and hands the non-PE script straight to
    // CreateProcessW, which can't execute it (and can't run a .cmd directly
    // either). Launch via `cmd /c` so cmd performs PATHEXT resolution and
    // finds `claude.cmd`. (Verified on a standard nvm-for-Windows install.)
    if cfg!(windows) {
        vec!["cmd".to_string(), "/c".to_string(), "claude".to_string()]
    } else {
        vec!["claude".to_string()]
    }
}

fn default_template() -> String {
    "claude-code".to_string()
}

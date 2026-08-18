//! Default configuration generation for first-run experience.

/// Generate a default JSONC config file content with helpful comments.
pub fn default_config_content() -> String {
    // A few sections only make sense on one platform: shell domains, window
    // backdrops and the tmux transport all differ. Generate the variant that
    // matches the host so a first-run config never documents WSL on Linux (or
    // xdg-open on Windows).
    let domain_help = if cfg!(windows) {
        r#"{DOMAIN_HELP}"#
    } else {
        r#"    // Default domain (shell). "local" runs your login shell ($SHELL).
    // "default_domain": "local",
    //
    // Default program for new panes. Element 0 is the executable, the rest are
    // arguments. Omit for your login shell.
    // "default_prog": ["/bin/bash", "-l"],
"#
    };

    let backdrop_help = if cfg!(windows) {
        r#"{BACKDROP_HELP}"#
    } else {
        r#"    // Window transparency. Requires a compositor that supports it
    // (most Wayland compositors and X11 with a compositing WM).
    // Background opacity: 0.0 (fully transparent) to 1.0 (opaque). Default: 0.85
    // "window_background_opacity": 0.85,
"#
    };

    let agent_help = if cfg!(windows) {
        r#"{AGENT_HELP}"#
    } else {
        r#"    //     "worktree_root": "/home/you/dev/worktrees",  // default: <repo-parent>/<repo>-worktrees/
    //     "claude_command": ["claude"],
"#
    };

    let tmux_boxes = if cfg!(windows) {
        r#"{TMUX_BOXES}"#
    } else {
        r#"    //         // Local tmux uses the "Command" escape hatch with an empty
    //         // prefix, so the tmux command runs directly on this machine.
    //         { "name": "local",  "connection": { "Command": { "argv_prefix": [] } } },
    //         { "name": "devbox", "connection": { "Ssh": { "target": "devbox" } } }
"#
    };

    let template = r#"{
    // Terminaler Configuration
    // Documentation: https://github.com/user/terminaler
    //
    // This file uses JSONC format (JSON with comments).
    // Lines starting with // are comments and will be ignored.

{DOMAIN_HELP}
    // What happens to a pane when its program exits.
    // "Close" (default) always closes the pane; "CloseOnCleanExit" closes it
    // only if the program succeeded, keeping it open on a non-zero exit so you
    // can read the error; "Hold" keeps it open until you close it explicitly.
    // Worth setting to "Hold" if default_prog runs something that can fail for
    // external reasons (e.g. ssh to a host that may be unreachable): with
    // "Close", the startup pane closes on failure, and when it is the last pane
    // that takes the window and the app with it — which looks like a crash at
    // startup rather than a failed command.
    // "exit_behavior": "Hold",

    // Font settings
    // "font_size": 12.0,

    // Window settings
    // "initial_rows": 24,
    // "initial_cols": 80,

{BACKDROP_HELP}
    // Color scheme. The default is "Gruvbox Dark (Gogh)" — a warm, low-strain
    // dark palette (cream text on a soft dark grey, not pure black). Set any of
    // the ~1000 bundled schemes here, e.g. "Tokyo Night", "Solarized Dark (Gogh)",
    // "Catppuccin Mocha", or a light scheme like "Catppuccin Latte".
    // Tip: press ctrl+shift+k (or "Theme Picker" in the command palette / View
    // menu) to browse and apply a theme live — your choice is written back here.
    // "color_scheme": "Gruvbox Dark (Gogh)",

    // Scrollback buffer size (number of lines)
    // "scrollback_lines": 10000,

    // Key bindings
    // "keys": [
    //     {
    //         "key": "ctrl+shift+l",
    //         "action": { "SnapLayoutPicker": null }
    //     },
    //     {
    //         "key": "ctrl+shift+t",
    //         "action": { "SpawnTab": "CurrentPaneDomain" }
    //     }
    // ]

    // Web access: view and control terminals from a browser
    // "web_access": {
    //     "enabled": true,
    //     "bind_address": "127.0.0.1:9876"
    //     // "token": "your-secret-token"  // auto-generated if omitted
    // }

    // Slack webhook for mobile/remote notifications (e.g., Claude Code)
    // "slack_notification_webhook": "https://hooks.slack.com/services/T.../B.../..."

    // Claude usage budgets (USD) — shown in sidebar stats card
    // "claude_daily_budget_usd": 5.0,
    // "claude_weekly_budget_usd": 25.0

    // Claude agent orchestration (ctrl+shift+a = New Agent, ctrl+shift+j = dashboard)
    // "claude_agent": {
{AGENT_HELP}    //     "template": "claude-code"
    // }

    // Multibox tmux sessions (ctrl+shift+s = session picker; sidebar section).
    // Attaching uses tmux control mode: tmux windows become real tabs.
    // "tmux": {
    //     "poll_interval_seconds": 30,
    //     "boxes": [
{TMUX_BOXES}    //     ]
    // }
}
"#;

    template
        .replace("{DOMAIN_HELP}", domain_help)
        .replace("{BACKDROP_HELP}", backdrop_help)
        .replace("{AGENT_HELP}", agent_help)
        .replace("{TMUX_BOXES}", tmux_boxes)
}

/// Get the default config file path for the current platform.
pub fn default_config_path() -> std::path::PathBuf {
    if let Some(ref dir) = *crate::PORTABLE_DIR {
        return dir.join("terminaler.json");
    }
    if cfg!(windows) {
        let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(appdata)
            .join("Terminaler")
            .join("terminaler.json")
    } else {
        dirs_next::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".config"))
            .join("terminaler")
            .join("terminaler.json")
    }
}

/// Ensure a config file exists. If none exists, create a default one.
/// Returns the path to the config file.
pub fn ensure_config_exists() -> anyhow::Result<std::path::PathBuf> {
    let path = default_config_path();
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, default_config_content())?;
        log::info!("Created default config at {}", path.display());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_is_valid_json() {
        let content = default_config_content();
        // Strip comments and verify it's valid JSON
        let stripped = crate::jsonc::strip_jsonc_comments(&content);
        let result: Result<serde_json::Value, _> = serde_json::from_str(&stripped);
        assert!(
            result.is_ok(),
            "Default config should be valid JSONC: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_default_config_path() {
        let path = default_config_path();
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains("terminaler") || path_str.contains("Terminaler"),
            "Config path should contain 'terminaler': {}",
            path_str
        );
    }
}

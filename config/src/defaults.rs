//! Default configuration generation for first-run experience.

/// Generate a default JSONC config file content with helpful comments.
pub fn default_config_content() -> String {
    r#"{
    // Terminaler Configuration
    // Documentation: https://github.com/user/terminaler
    //
    // This file uses JSONC format (JSON with comments).
    // Lines starting with // are comments and will be ignored.

    // Default domain (shell). WSL is used automatically if available.
    // Set to "local" for PowerShell/CMD, or "WSL:<distro>" for a specific WSL distro.
    // "default_domain": "local",

    // Font settings
    // "font_size": 12.0,

    // Window settings
    // "initial_rows": 24,
    // "initial_cols": 80,

    // Window transparency (Windows 11 glass effect)
    // Backdrop type: "Auto", "Disable", "Acrylic", "Mica", "Tabbed"
    // Default: "Acrylic"
    // "win32_system_backdrop": "Acrylic",
    // Background opacity: 0.0 (fully transparent) to 1.0 (opaque). Default: 0.85
    // "window_background_opacity": 0.85,

    // Color scheme: use built-in "dark" or "light" themes
    // or define custom colors below
    // "color_scheme": "dark",

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
}
"#
    .to_string()
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
